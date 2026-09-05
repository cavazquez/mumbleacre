//! Typed parser for ACRE2's `updateSpeakingData` decision messages.
//!
//! ACRE decides audibility in SQF and emits this message per remote speaker.
//! The parser is pure so a later worker can turn its result into immutable RT
//! snapshots without making radio/frequency decisions a second time.

use thiserror::Error;

use crate::AcreMessage;

/// Bound work and snapshot memory per radio speaker. ACRE may return several
/// reception paths, but the adapter never lets one message force unbounded
/// allocation before it reaches the control-plane snapshot.
pub const ACRE_MAX_RADIO_RECEPTION_PATHS: usize = 64;

/// Coordinate ordering used by ACRE's mixer parameters (`x`, `z`, `y`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcreSpeakerVector {
    pub x: f32,
    pub z: f32,
    pub y: f32,
}

/// Listener pose supplied by ACRE's `updateSelf` RPC.
///
/// The wire order remains `x`, `z`, `y`, matching the values handed to the
/// stock voice-plugin mixer. It is relayed unchanged so the eventual Mumble
/// renderer can derive spatial direction without consulting the legacy Link
/// bridge or recomputing ACRE's audibility decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcreListenerPose {
    pub position: AcreSpeakerVector,
    pub head_vector: AcreSpeakerVector,
}

/// ACRE's v2.14 listener-side direct-voice curve selection.
///
/// The numeric values match `acre::CurveModel` in the pinned ACRE2 source.
/// The renderer consumes this setting together with a remote speaker's curve
/// scale; it never guesses radio availability or signal quality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AcreVoiceCurveModel {
    Original = 0,
    Amplitude = 1,
    SelectableA = 2,
    SelectableB = 3,
}

impl TryFrom<i32> for AcreVoiceCurveModel {
    type Error = AcreSpeakingDataError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Original),
            1 => Ok(Self::Amplitude),
            2 => Ok(Self::SelectableA),
            3 => Ok(Self::SelectableB),
            model => Err(AcreSpeakingDataError::InvalidVoiceCurveModel { model }),
        }
    }
}

/// Complete local state required to spatially render one ACRE decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcreListenerState {
    pub pose: AcreListenerPose,
    pub curve_model: AcreVoiceCurveModel,
}

/// Direct-like speaking modes encoded by `updateSpeakingData`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialSpeakingKind {
    Direct,
    Intercom,
    Zeus,
}

/// One reception path for a radio transmission.
#[derive(Clone, Debug, PartialEq)]
pub struct RadioReceptionPath {
    pub volume: f32,
    pub signal_quality: f32,
    pub signal_model: f32,
    pub loudspeaker: bool,
    pub position: AcreSpeakerVector,
}

/// ACRE's audibility decision for one Mumble speaker.
#[derive(Clone, Debug, PartialEq)]
pub enum SpeakingDecision {
    Mute,
    Spatial {
        kind: SpatialSpeakingKind,
        volume: f32,
        position: AcreSpeakerVector,
        head_vector: AcreSpeakerVector,
    },
    Radio {
        paths: Vec<RadioReceptionPath>,
    },
    Spectator {
        volume: f32,
    },
    God {
        volume: f32,
    },
}

/// One validated `updateSpeakingData` message.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeakingUpdate {
    pub speaker_id: u32,
    pub speaks_babel: bool,
    pub decision: SpeakingDecision,
}

/// A parsed remote decision paired with the sender's already authenticated
/// curve scale from peer control. `updateSpeakingData` itself has no curve
/// scale field, so the parser starts at the stock default (`1.0`) and the
/// session replaces it only from validated remote transmission state.
#[derive(Clone, Debug, PartialEq)]
pub struct AcreSpeakerAudioUpdate {
    pub speaking: SpeakingUpdate,
    pub curve_scale: f32,
}

impl AcreSpeakerAudioUpdate {
    pub fn new(speaking: SpeakingUpdate, curve_scale: f32) -> Result<Self, AcreSpeakingDataError> {
        if !curve_scale.is_finite() {
            return Err(AcreSpeakingDataError::InvalidCurveScale);
        }
        Ok(Self {
            speaking,
            curve_scale,
        })
    }
}

impl From<SpeakingUpdate> for AcreSpeakerAudioUpdate {
    fn from(speaking: SpeakingUpdate) -> Self {
        Self {
            speaking,
            curve_scale: 1.0,
        }
    }
}

/// Parses the ACRE2 v2.14.0.1064 audio decision format.
pub fn parse_update_speaking_data(
    message: &AcreMessage,
) -> Result<SpeakingUpdate, AcreSpeakingDataError> {
    if message.procedure() != "updateSpeakingData" {
        return Err(AcreSpeakingDataError::UnexpectedProcedure(
            message.procedure().to_owned(),
        ));
    }
    require_minimum_arity(message, 3)?;
    let speaker_id = parse_u32(message, 1)?;
    let speaks_babel = parse_binary_flag(message, 2)?;

    match message.parameters()[0].as_str() {
        "m" => {
            require_exact_arity(message, 3)?;
            Ok(SpeakingUpdate {
                speaker_id,
                speaks_babel,
                decision: SpeakingDecision::Mute,
            })
        }
        "d" | "i" | "z" => parse_spatial(message, speaker_id, speaks_babel),
        "r" => parse_radio(message, speaker_id, speaks_babel),
        "s" => parse_monitor(message, speaker_id, speaks_babel, false),
        "g" => parse_monitor(message, speaker_id, speaks_babel, true),
        kind => Err(AcreSpeakingDataError::UnsupportedSpeakingKind(
            kind.to_owned(),
        )),
    }
}

fn parse_spatial(
    message: &AcreMessage,
    speaker_id: u32,
    speaks_babel: bool,
) -> Result<SpeakingUpdate, AcreSpeakingDataError> {
    require_exact_arity(message, 10)?;
    let kind = match message.parameters()[0].as_str() {
        "d" => SpatialSpeakingKind::Direct,
        "i" => SpatialSpeakingKind::Intercom,
        "z" => SpatialSpeakingKind::Zeus,
        _ => unreachable!("spatial kind checked by caller"),
    };
    Ok(SpeakingUpdate {
        speaker_id,
        speaks_babel,
        decision: SpeakingDecision::Spatial {
            kind,
            volume: parse_finite_f32(message, 3)?,
            position: parse_vector(message, 4)?,
            head_vector: parse_vector(message, 7)?,
        },
    })
}

fn parse_radio(
    message: &AcreMessage,
    speaker_id: u32,
    speaks_babel: bool,
) -> Result<SpeakingUpdate, AcreSpeakingDataError> {
    require_minimum_arity(message, 4)?;
    let path_count = parse_usize(message, 3)?;
    if path_count > ACRE_MAX_RADIO_RECEPTION_PATHS {
        return Err(AcreSpeakingDataError::RadioPathCountTooLarge);
    }
    let expected_arity = 4_usize
        .checked_add(
            path_count
                .checked_mul(7)
                .ok_or(AcreSpeakingDataError::RadioPathCountTooLarge)?,
        )
        .ok_or(AcreSpeakingDataError::RadioPathCountTooLarge)?;
    require_exact_arity(message, expected_arity)?;

    let mut paths = Vec::with_capacity(path_count);
    for path_index in 0..path_count {
        let offset = 4 + (path_index * 7);
        paths.push(RadioReceptionPath {
            volume: parse_finite_f32(message, offset)?,
            signal_quality: parse_finite_f32(message, offset + 1)?,
            signal_model: parse_finite_f32(message, offset + 2)?,
            loudspeaker: parse_finite_f32(message, offset + 3)? != 0.0,
            position: parse_vector(message, offset + 4)?,
        });
    }
    Ok(SpeakingUpdate {
        speaker_id,
        speaks_babel,
        decision: SpeakingDecision::Radio { paths },
    })
}

fn parse_monitor(
    message: &AcreMessage,
    speaker_id: u32,
    speaks_babel: bool,
    god: bool,
) -> Result<SpeakingUpdate, AcreSpeakingDataError> {
    require_exact_arity(message, 4)?;
    let volume = parse_finite_f32(message, 3)?;
    let decision = if god {
        SpeakingDecision::God { volume }
    } else {
        SpeakingDecision::Spectator { volume }
    };
    Ok(SpeakingUpdate {
        speaker_id,
        speaks_babel,
        decision,
    })
}

fn parse_vector(
    message: &AcreMessage,
    start_index: usize,
) -> Result<AcreSpeakerVector, AcreSpeakingDataError> {
    Ok(AcreSpeakerVector {
        x: parse_finite_f32(message, start_index)?,
        z: parse_finite_f32(message, start_index + 1)?,
        y: parse_finite_f32(message, start_index + 2)?,
    })
}

fn require_minimum_arity(
    message: &AcreMessage,
    minimum: usize,
) -> Result<(), AcreSpeakingDataError> {
    if message.parameters().len() < minimum {
        return Err(AcreSpeakingDataError::WrongArity {
            expected: format!("at least {minimum}"),
            actual: message.parameters().len(),
        });
    }
    Ok(())
}

fn require_exact_arity(
    message: &AcreMessage,
    expected: usize,
) -> Result<(), AcreSpeakingDataError> {
    if message.parameters().len() != expected {
        return Err(AcreSpeakingDataError::WrongArity {
            expected: expected.to_string(),
            actual: message.parameters().len(),
        });
    }
    Ok(())
}

fn parse_u32(message: &AcreMessage, index: usize) -> Result<u32, AcreSpeakingDataError> {
    message.parameters()[index]
        .parse::<u32>()
        .map_err(|_| AcreSpeakingDataError::InvalidUnsigned { index })
}

fn parse_usize(message: &AcreMessage, index: usize) -> Result<usize, AcreSpeakingDataError> {
    message.parameters()[index]
        .parse::<usize>()
        .map_err(|_| AcreSpeakingDataError::InvalidUnsigned { index })
}

fn parse_binary_flag(message: &AcreMessage, index: usize) -> Result<bool, AcreSpeakingDataError> {
    match message.parameters()[index].as_str() {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(AcreSpeakingDataError::InvalidBinaryFlag { index }),
    }
}

fn parse_finite_f32(message: &AcreMessage, index: usize) -> Result<f32, AcreSpeakingDataError> {
    let value = message.parameters()[index]
        .parse::<f32>()
        .map_err(|_| AcreSpeakingDataError::InvalidFloat { index })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AcreSpeakingDataError::InvalidFloat { index })
    }
}

/// Rejection reason for one untrusted `updateSpeakingData` message.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AcreSpeakingDataError {
    #[error("expected updateSpeakingData, received {0}")]
    UnexpectedProcedure(String),
    #[error("updateSpeakingData has {actual} parameters; expected {expected}")]
    WrongArity { expected: String, actual: usize },
    #[error("updateSpeakingData has unsupported speaking kind {0}")]
    UnsupportedSpeakingKind(String),
    #[error("updateSpeakingData parameter {index} must be an unsigned integer")]
    InvalidUnsigned { index: usize },
    #[error("updateSpeakingData parameter {index} must be exactly 0 or 1")]
    InvalidBinaryFlag { index: usize },
    #[error("updateSpeakingData parameter {index} must be a finite float")]
    InvalidFloat { index: usize },
    #[error("ACRE voice curve model {model} is unsupported")]
    InvalidVoiceCurveModel { model: i32 },
    #[error("ACRE speaker curve scale must be finite")]
    InvalidCurveScale,
    #[error("updateSpeakingData radio path count exceeds the bounded adapter limit")]
    RadioPathCountTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AcreMessage;

    fn update(text: &str) -> AcreMessage {
        AcreMessage::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn parses_direct_intercom_and_zeus_layouts() {
        let direct = parse_update_speaking_data(&update(
            "updateSpeakingData:d,42,1,0.75,100,300,200,0,1,0,",
        ))
        .unwrap();
        assert_eq!(direct.speaker_id, 42);
        assert!(direct.speaks_babel);
        assert_eq!(
            direct.decision,
            SpeakingDecision::Spatial {
                kind: SpatialSpeakingKind::Direct,
                volume: 0.75,
                position: AcreSpeakerVector {
                    x: 100.0,
                    z: 300.0,
                    y: 200.0,
                },
                head_vector: AcreSpeakerVector {
                    x: 0.0,
                    z: 1.0,
                    y: 0.0,
                },
            }
        );

        for (kind, expected) in [
            ("i", SpatialSpeakingKind::Intercom),
            ("z", SpatialSpeakingKind::Zeus),
        ] {
            let message = update(&format!("updateSpeakingData:{kind},42,0,1,0,0,0,0,1,0,"));
            let parsed = parse_update_speaking_data(&message).unwrap();
            assert!(matches!(
                parsed.decision,
                SpeakingDecision::Spatial { kind, .. } if kind == expected
            ));
        }
    }

    #[test]
    fn parses_all_radio_paths_without_recomputing_radio_logic() {
        let parsed = parse_update_speaking_data(&update(
            "updateSpeakingData:r,42,0,2,0.9,1,2,0,10,30,20,0.5,0.8,3,1,40,60,50,",
        ))
        .unwrap();
        assert_eq!(
            parsed.decision,
            SpeakingDecision::Radio {
                paths: vec![
                    RadioReceptionPath {
                        volume: 0.9,
                        signal_quality: 1.0,
                        signal_model: 2.0,
                        loudspeaker: false,
                        position: AcreSpeakerVector {
                            x: 10.0,
                            z: 30.0,
                            y: 20.0,
                        },
                    },
                    RadioReceptionPath {
                        volume: 0.5,
                        signal_quality: 0.8,
                        signal_model: 3.0,
                        loudspeaker: true,
                        position: AcreSpeakerVector {
                            x: 40.0,
                            z: 60.0,
                            y: 50.0,
                        },
                    },
                ],
            }
        );
    }

    #[test]
    fn parses_mute_spectator_and_god() {
        let mute = parse_update_speaking_data(&update("updateSpeakingData:m,8,0,")).unwrap();
        assert_eq!(mute.decision, SpeakingDecision::Mute);

        let spectator =
            parse_update_speaking_data(&update("updateSpeakingData:s,8,0,0.6,")).unwrap();
        assert_eq!(
            spectator.decision,
            SpeakingDecision::Spectator { volume: 0.6 }
        );

        let god = parse_update_speaking_data(&update("updateSpeakingData:g,8,0,0.9,")).unwrap();
        assert_eq!(god.decision, SpeakingDecision::God { volume: 0.9 });
    }

    #[test]
    fn rejects_bad_arity_counts_and_non_finite_values_without_a_partial_decision() {
        assert!(matches!(
            parse_update_speaking_data(&update("updateSpeakingData:r,42,0,2,0.9,1,2,0,10,30,20,")),
            Err(AcreSpeakingDataError::WrongArity { .. })
        ));
        assert!(matches!(
            parse_update_speaking_data(&update("updateSpeakingData:d,42,0,NaN,0,0,0,0,1,0,")),
            Err(AcreSpeakingDataError::InvalidFloat { index: 3 })
        ));
        assert!(matches!(
            parse_update_speaking_data(&update("updateSpeakingData:q,42,0,")),
            Err(AcreSpeakingDataError::UnsupportedSpeakingKind(_))
        ));
        let too_many_paths = format!(
            "updateSpeakingData:r,42,0,{},",
            ACRE_MAX_RADIO_RECEPTION_PATHS + 1
        );
        assert!(matches!(
            parse_update_speaking_data(&update(&too_many_paths)),
            Err(AcreSpeakingDataError::RadioPathCountTooLarge)
        ));
    }
}
