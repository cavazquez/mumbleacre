use std::collections::HashMap;
use std::time::Instant;

use thiserror::Error;

use crate::{
    AcreCodecError, AcreListenerPose, AcreListenerState, AcreLoadedSound, AcreMessage,
    AcreSoundAssembler, AcreSoundError, AcreSoundPlayback, AcreSpeakerAudioUpdate,
    AcreSpeakerVector, AcreSpeakingDataError, AcreVoiceCurveModel, parse_load_sound,
    parse_play_loaded_sound, parse_update_speaking_data,
};

/// The ACRE2 release whose private RPC contract is represented in this crate.
pub const ACRE2_PLUGIN_VERSION: &str = "2.14.0.1064";

/// Numeric values used by ACRE2's `acre::Speaking` enum in v2.14.0.1064.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SpeakingKind {
    Direct = 0,
    Radio = 1,
    Unknown = 2,
    Intercom = 3,
    Spectate = 4,
    God = 5,
    Zeus = 6,
}

impl SpeakingKind {
    #[must_use]
    pub const fn wire_value(self) -> u8 {
        self as u8
    }
}

/// Metadata ACRE needs for one local or remote transmission.
#[derive(Clone, Debug, PartialEq)]
pub struct Transmission {
    voice_client_id: u32,
    language_id: i32,
    net_id: String,
    speaking_kind: SpeakingKind,
    radio_id: String,
    curve_scale: f32,
}

/// A [`Transmission`] received from another compatible Mumble adapter.
pub type RemoteTransmission = Transmission;

/// Mumble connection metadata that ACRE exposes to mission code through its
/// historical VOIP queries. Values are already encoded for ACRE's ASCII RPC
/// boundary; they are not raw Mumble strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoipMetadata {
    server_name: String,
    server_uid: String,
    channel_name: String,
    channel_uid: String,
}

impl VoipMetadata {
    /// Builds bounded metadata suitable for one ACRE RPC parameter per value.
    pub fn new(
        server_name: impl Into<String>,
        server_uid: impl Into<String>,
        channel_name: impl Into<String>,
        channel_uid: impl Into<String>,
    ) -> Result<Self, AcreSessionError> {
        let metadata = Self {
            server_name: server_name.into(),
            server_uid: server_uid.into(),
            channel_name: channel_name.into(),
            channel_uid: channel_uid.into(),
        };
        validate_voip_metadata_field("server_name", &metadata.server_name)?;
        validate_voip_metadata_field("server_uid", &metadata.server_uid)?;
        validate_voip_metadata_field("channel_name", &metadata.channel_name)?;
        validate_voip_metadata_field("channel_uid", &metadata.channel_uid)?;
        Ok(metadata)
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    #[must_use]
    pub fn server_uid(&self) -> &str {
        &self.server_uid
    }

    #[must_use]
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    #[must_use]
    pub fn channel_uid(&self) -> &str {
        &self.channel_uid
    }
}

impl Transmission {
    /// Creates metadata that can later be translated to ACRE's
    /// `remoteStartSpeaking` RPC.
    pub fn new(
        voice_client_id: u32,
        language_id: i32,
        net_id: impl Into<String>,
        speaking_kind: SpeakingKind,
        radio_id: impl Into<String>,
        curve_scale: f32,
    ) -> Result<Self, AcreSessionError> {
        if !curve_scale.is_finite() {
            return Err(AcreSessionError::NonFiniteCurveScale);
        }
        Ok(Self {
            voice_client_id,
            language_id,
            net_id: net_id.into(),
            speaking_kind,
            radio_id: radio_id.into(),
            curve_scale,
        })
    }

    #[must_use]
    pub const fn voice_client_id(&self) -> u32 {
        self.voice_client_id
    }

    #[must_use]
    pub const fn language_id(&self) -> i32 {
        self.language_id
    }

    #[must_use]
    pub fn net_id(&self) -> &str {
        &self.net_id
    }

    #[must_use]
    pub const fn speaking_kind(&self) -> SpeakingKind {
        self.speaking_kind
    }

    #[must_use]
    pub fn radio_id(&self) -> &str {
        &self.radio_id
    }

    #[must_use]
    pub const fn curve_scale(&self) -> f32 {
        self.curve_scale
    }
}

/// Result of interpreting one message that arrived from `ACRE2Arma`.
///
/// `SendToArma` stays on the pipe worker.  The two local transmission actions
/// deliberately contain no Mumble call; a later control-plane issue consumes
/// them outside the real-time callback. `ListenerUpdated` and `SpeakingUpdated`
/// carry validated spatial state to the immutable RT snapshot publisher, still
/// outside audio. Loaded sounds are assembled here; their WAV preparation stays
/// on a dedicated non-RT worker and playback is handed back to a Mumble plugin
/// callback instead of being invoked by that worker.
#[derive(Clone, Debug, PartialEq)]
pub enum AcreAction {
    SendToArma(AcreMessage),
    LocalTransmissionStarted(Transmission),
    LocalTransmissionStopped(Transmission),
    ListenerUpdated(AcreListenerState),
    SpeakingUpdated(AcreSpeakerAudioUpdate),
    SoundLoaded(AcreLoadedSound),
    SoundPlaybackRequested(AcreSoundPlayback),
    ResetRequested,
}

/// Minimal ACRE voice-plugin state needed for the first compatibility slice.
#[derive(Debug)]
pub struct AcreSession {
    plugin_version: String,
    local_voice_client_id: Option<u32>,
    voip_metadata: Option<VoipMetadata>,
    local_net_id: Option<String>,
    local_language_id: i32,
    local_listener_pose: Option<AcreListenerPose>,
    local_voice_curve_model: AcreVoiceCurveModel,
    local_curve_scale: f32,
    active_local_transmission: Option<Transmission>,
    remote_transmissions: HashMap<u32, RemoteTransmission>,
    sound_assembler: AcreSoundAssembler,
}

impl Default for AcreSession {
    fn default() -> Self {
        Self::new(ACRE2_PLUGIN_VERSION)
    }
}

impl AcreSession {
    #[must_use]
    pub fn new(plugin_version: impl Into<String>) -> Self {
        Self {
            plugin_version: plugin_version.into(),
            local_voice_client_id: None,
            voip_metadata: None,
            local_net_id: None,
            local_language_id: 0,
            local_listener_pose: None,
            local_voice_curve_model: AcreVoiceCurveModel::Original,
            local_curve_scale: 1.0,
            active_local_transmission: None,
            remote_transmissions: HashMap::new(),
            sound_assembler: AcreSoundAssembler::default(),
        }
    }

    /// Supplies the Mumble session ID captured on a synchronized connection.
    pub fn set_local_voice_client_id(&mut self, voice_client_id: Option<u32>) {
        self.local_voice_client_id = voice_client_id;
    }

    /// Supplies a snapshot captured by the Mumble control plane. The pipe
    /// worker owns no Mumble API calls and replaces this snapshot before
    /// dispatching each incoming ACRE RPC.
    pub fn set_voip_metadata(&mut self, metadata: Option<VoipMetadata>) {
        self.voip_metadata = metadata;
    }

    /// Supplies the ACRE/Babel language ID for subsequent local starts.
    pub fn set_local_language_id(&mut self, language_id: i32) {
        self.local_language_id = language_id;
    }

    /// Supplies the selectable voice-curve scale for subsequent local starts.
    pub fn set_local_curve_scale(&mut self, curve_scale: f32) -> Result<(), AcreSessionError> {
        if !curve_scale.is_finite() {
            return Err(AcreSessionError::NonFiniteCurveScale);
        }
        self.local_curve_scale = curve_scale;
        Ok(())
    }

    #[must_use]
    pub fn local_net_id(&self) -> Option<&str> {
        self.local_net_id.as_deref()
    }

    /// Current native Mumble voice metadata, only available after ACRE identity.
    pub fn direct_transmission(&self) -> Option<Transmission> {
        Transmission::new(
            self.local_voice_client_id?,
            self.local_language_id,
            self.local_net_id.as_ref()?,
            SpeakingKind::Direct,
            "",
            self.local_curve_scale,
        )
        .ok()
    }

    #[must_use]
    pub fn active_local_transmission(&self) -> Option<&Transmission> {
        self.active_local_transmission.as_ref()
    }

    /// Clears state that cannot survive a broken pipe, reconnect, or Mumble
    /// identity change.  The next ACRE `getClientID` establishes the local
    /// netId again before another PTT can start.
    pub fn reset(&mut self) {
        self.local_language_id = 0;
        self.local_curve_scale = 1.0;
        self.local_net_id = None;
        self.local_listener_pose = None;
        self.local_voice_curve_model = AcreVoiceCurveModel::Original;
        self.active_local_transmission = None;
        self.remote_transmissions.clear();
        self.sound_assembler.clear();
    }

    /// Handles the part of the ACRE RPC surface required by the AM-01 slice.
    ///
    /// `elapsed_seconds` is injected so `ping` can be deterministic in tests.
    /// ACRE's stock implementation formats its process clock with `%f`.
    pub fn handle_from_arma(
        &mut self,
        message: &AcreMessage,
        elapsed_seconds: f64,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        if !elapsed_seconds.is_finite() {
            return Err(AcreSessionError::NonFiniteClock);
        }

        match message.procedure() {
            "ping" => {
                expect_arity(message, 0)?;
                Ok(vec![AcreAction::SendToArma(message_with_comma(
                    "pong",
                    vec![format!("{elapsed_seconds:.6}")],
                )?)])
            }
            "getPluginVersion" => {
                expect_arity(message, 0)?;
                Ok(vec![AcreAction::SendToArma(AcreMessage::new(
                    "handleGetPluginVersion",
                    vec![self.plugin_version.clone()],
                )?)])
            }
            "getClientID" => self.handle_get_client_id(message),
            "updateSelf" => self.update_self(message),
            "setSelectableVoiceCurve" => self.set_selectable_voice_curve(message),
            "setVoiceCurveModel" => self.set_voice_curve_model(message),
            "setSetting" => Self::validate_setting(message),
            "setTs3ChannelDetails" => {
                expect_arity(message, 3)?;
                Ok(Vec::new())
            }
            "loadSound" => self.load_sound(message),
            "playLoadedSound" => Self::play_loaded_sound(message),
            "getVOIPServerName" => {
                self.voip_reply(message, "handleGetVOIPServerName", |metadata| {
                    metadata.server_name()
                })
            }
            "getVOIPServerUID" => self.voip_reply(message, "handleGetVOIPServerUID", |metadata| {
                metadata.server_uid()
            }),
            "getVOIPChannelName" => {
                self.voip_reply(message, "handleGetVOIPChannelName", |metadata| {
                    metadata.channel_name()
                })
            }
            "getVOIPChannelUID" => {
                self.voip_reply(message, "handleGetVOIPChannelUID", |metadata| {
                    metadata.channel_uid()
                })
            }
            // The pipe worker forwards this parsed decision to the immutable
            // control-plane snapshot. It still never touches PCM or calls a
            // Mumble API from the worker.
            "updateSpeakingData" => self.update_speaking_data(message),
            "startRadioSpeaking" => self.start_local(message, SpeakingKind::Radio),
            "stopRadioSpeaking" => self.stop_local(message, SpeakingKind::Radio),
            "startIntercomSpeaking" => self.start_local(message, SpeakingKind::Intercom),
            "stopIntercomSpeaking" => self.stop_local(message, SpeakingKind::Intercom),
            "startGodModeSpeaking" => self.start_local(message, SpeakingKind::God),
            "stopGodModeSpeaking" => self.stop_local(message, SpeakingKind::God),
            "startZeusSpeaking" => self.start_local(message, SpeakingKind::Zeus),
            "stopZeusSpeaking" => self.stop_local(message, SpeakingKind::Zeus),
            "ext_reset" => {
                if message.parameters().len() > 1 {
                    return Err(AcreSessionError::WrongArity {
                        procedure: message.procedure().to_owned(),
                        expected: "zero or one",
                        actual: message.parameters().len(),
                    });
                }
                self.reset();
                Ok(vec![AcreAction::ResetRequested])
            }
            procedure => Err(AcreSessionError::UnsupportedProcedure(procedure.to_owned())),
        }
    }

    /// Translates an already validated peer START to ACRE's game-side RPC.
    pub fn remote_started(
        &mut self,
        transmission: &RemoteTransmission,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        let previous = self
            .remote_transmissions
            .get(&transmission.voice_client_id)
            .cloned();
        if previous.as_ref() == Some(transmission) {
            return Ok(Vec::new());
        }

        // Build every outbound RPC before mutating the tracked state. A
        // programmatic caller might bypass the peer decoder, so a serialization
        // error must not leave a remote transmission pinned in this session.
        let mut actions = Vec::new();
        if let Some(previous) = previous.as_ref() {
            actions.push(AcreAction::SendToArma(remote_stop_message(previous)?));
        }
        let start = remote_start_message(transmission)?;
        self.remote_transmissions
            .insert(transmission.voice_client_id, transmission.clone());
        actions.push(AcreAction::SendToArma(start));
        Ok(actions)
    }

    /// Translates an already validated peer STOP. Duplicate stops are benign.
    pub fn remote_stopped(
        &mut self,
        voice_client_id: u32,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        let Some(transmission) = self.remote_transmissions.remove(&voice_client_id) else {
            return Ok(Vec::new());
        };
        Ok(vec![AcreAction::SendToArma(remote_stop_message(
            &transmission,
        )?)])
    }

    fn handle_get_client_id(
        &mut self,
        message: &AcreMessage,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 1)?;
        let net_id = message.parameters()[0].clone();
        let voice_client_id = self
            .local_voice_client_id
            .ok_or(AcreSessionError::LocalVoiceClientIdUnavailable)?;
        self.local_net_id = Some(net_id.clone());
        Ok(vec![AcreAction::SendToArma(message_with_comma(
            "handleGetClientID",
            vec![voice_client_id.to_string(), net_id],
        )?)])
    }

    fn voip_reply(
        &self,
        message: &AcreMessage,
        reply_procedure: &str,
        value: impl FnOnce(&VoipMetadata) -> &str,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 0)?;
        let metadata = self
            .voip_metadata
            .as_ref()
            .ok_or(AcreSessionError::VoipMetadataUnavailable)?;
        Ok(vec![AcreAction::SendToArma(AcreMessage::new(
            reply_procedure,
            vec![value(metadata).to_owned()],
        )?)])
    }

    fn update_self(&mut self, message: &AcreMessage) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 7)?;
        // Parse every spatial field before mutating the language state so a
        // malformed message cannot publish a partial listener pose.
        let pose = AcreListenerPose {
            position: AcreSpeakerVector {
                x: parse_finite_float(message, 0)?,
                z: parse_finite_float(message, 1)?,
                y: parse_finite_float(message, 2)?,
            },
            head_vector: AcreSpeakerVector {
                x: parse_finite_float(message, 3)?,
                z: parse_finite_float(message, 4)?,
                y: parse_finite_float(message, 5)?,
            },
        };
        let language_id = parse_i32(message, 6)?;
        let state = AcreListenerState {
            pose,
            curve_model: self.local_voice_curve_model,
        };
        self.local_language_id = language_id;
        self.local_listener_pose = Some(pose);
        Ok(vec![AcreAction::ListenerUpdated(state)])
    }

    fn set_selectable_voice_curve(
        &mut self,
        message: &AcreMessage,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 1)?;
        self.set_local_curve_scale(parse_finite_float(message, 0)?)?;
        Ok(Vec::new())
    }

    fn set_voice_curve_model(
        &mut self,
        message: &AcreMessage,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 2)?;
        let model = AcreVoiceCurveModel::try_from(parse_i32(message, 0)?)?;
        let _ = parse_finite_float(message, 1)?;
        self.local_voice_curve_model = model;
        Ok(self
            .local_listener_state()
            .map(AcreAction::ListenerUpdated)
            .into_iter()
            .collect())
    }

    fn validate_setting(message: &AcreMessage) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 2)?;
        if message.parameters()[0].is_empty() {
            return Err(AcreSessionError::EmptySettingName);
        }
        let _ = parse_finite_float(message, 1)?;
        Ok(Vec::new())
    }

    fn update_speaking_data(
        &self,
        message: &AcreMessage,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        let speaking = parse_update_speaking_data(message)?;
        let curve_scale = self
            .remote_transmissions
            .get(&speaking.speaker_id)
            .map_or(1.0, Transmission::curve_scale);
        Ok(vec![AcreAction::SpeakingUpdated(
            AcreSpeakerAudioUpdate::new(speaking, curve_scale)?,
        )])
    }

    fn load_sound(&mut self, message: &AcreMessage) -> Result<Vec<AcreAction>, AcreSessionError> {
        let chunk = parse_load_sound(message)?;
        Ok(self
            .sound_assembler
            .accept(chunk, Instant::now())?
            .map(AcreAction::SoundLoaded)
            .into_iter()
            .collect())
    }

    fn play_loaded_sound(message: &AcreMessage) -> Result<Vec<AcreAction>, AcreSessionError> {
        Ok(vec![AcreAction::SoundPlaybackRequested(
            parse_play_loaded_sound(message)?,
        )])
    }

    /// Expires incomplete `loadSound` transfers from the pipe worker's regular
    /// control tick. The returned IDs are diagnostics only; ACRE has no stock
    /// timeout callback for partial base64 uploads.
    pub fn expire_sound_loads(&mut self, now: Instant) -> Vec<String> {
        self.sound_assembler.expire(now)
    }

    fn local_listener_state(&self) -> Option<AcreListenerState> {
        self.local_listener_pose.map(|pose| AcreListenerState {
            pose,
            curve_model: self.local_voice_curve_model,
        })
    }

    fn start_local(
        &mut self,
        message: &AcreMessage,
        speaking_kind: SpeakingKind,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        let radio_id = match speaking_kind {
            SpeakingKind::Radio => {
                expect_arity(message, 1)?;
                let radio_id = message.parameters()[0].clone();
                if radio_id.is_empty() {
                    return Err(AcreSessionError::EmptyRadioId);
                }
                radio_id
            }
            SpeakingKind::Intercom | SpeakingKind::God | SpeakingKind::Zeus => {
                expect_arity(message, 0)?;
                String::new()
            }
            _ => unreachable!("only ACRE-controlled PTT modes reach start_local"),
        };

        let transmission = Transmission::new(
            self.local_voice_client_id
                .ok_or(AcreSessionError::LocalVoiceClientIdUnavailable)?,
            self.local_language_id,
            self.local_net_id
                .clone()
                .filter(|net_id| !net_id.is_empty())
                .ok_or(AcreSessionError::LocalNetIdUnavailable)?,
            speaking_kind,
            radio_id,
            self.local_curve_scale,
        )?;

        if self.active_local_transmission.as_ref() == Some(&transmission) {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        if let Some(previous) = self.active_local_transmission.take() {
            actions.extend(stop_actions(&previous)?);
        }
        self.active_local_transmission = Some(transmission.clone());
        actions.push(AcreAction::SendToArma(local_start_message(&transmission)?));
        actions.push(AcreAction::LocalTransmissionStarted(transmission));
        Ok(actions)
    }

    fn stop_local(
        &mut self,
        message: &AcreMessage,
        speaking_kind: SpeakingKind,
    ) -> Result<Vec<AcreAction>, AcreSessionError> {
        expect_arity(message, 0)?;
        let Some(active) = self.active_local_transmission.as_ref() else {
            return Ok(Vec::new());
        };
        if active.speaking_kind != speaking_kind {
            return Ok(Vec::new());
        }
        let active = self
            .active_local_transmission
            .take()
            .expect("active transmission checked above");
        stop_actions(&active)
    }
}

const MAX_VOIP_METADATA_FIELD_BYTES: usize = 512;

fn validate_voip_metadata_field(field: &'static str, value: &str) -> Result<(), AcreSessionError> {
    if value.is_empty() {
        return Err(AcreSessionError::EmptyVoipMetadataField(field));
    }
    if value.len() > MAX_VOIP_METADATA_FIELD_BYTES {
        return Err(AcreSessionError::VoipMetadataFieldTooLong {
            field,
            length: value.len(),
            limit: MAX_VOIP_METADATA_FIELD_BYTES,
        });
    }
    if value
        .bytes()
        .any(|byte| !byte.is_ascii() || byte == b',' || byte == 0 || byte.is_ascii_control())
    {
        return Err(AcreSessionError::InvalidVoipMetadataField(field));
    }
    Ok(())
}

fn expect_arity(message: &AcreMessage, expected: usize) -> Result<(), AcreSessionError> {
    if message.parameters().len() == expected {
        Ok(())
    } else {
        Err(AcreSessionError::WrongArity {
            procedure: message.procedure().to_owned(),
            expected: match expected {
                0 => "zero",
                1 => "one",
                _ => "the expected number of",
            },
            actual: message.parameters().len(),
        })
    }
}

fn parse_i32(message: &AcreMessage, index: usize) -> Result<i32, AcreSessionError> {
    message.parameters()[index]
        .parse::<i32>()
        .map_err(|_| AcreSessionError::InvalidInteger {
            procedure: message.procedure().to_owned(),
            index,
        })
}

fn parse_finite_float(message: &AcreMessage, index: usize) -> Result<f32, AcreSessionError> {
    let value =
        message.parameters()[index]
            .parse::<f32>()
            .map_err(|_| AcreSessionError::InvalidFloat {
                procedure: message.procedure().to_owned(),
                index,
            })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AcreSessionError::InvalidFloat {
            procedure: message.procedure().to_owned(),
            index,
        })
    }
}

fn message_with_comma(
    procedure: &str,
    parameters: Vec<String>,
) -> Result<AcreMessage, AcreSessionError> {
    Ok(AcreMessage::with_trailing_comma(
        procedure, parameters, true,
    )?)
}

fn local_start_message(transmission: &Transmission) -> Result<AcreMessage, AcreSessionError> {
    message_with_comma(
        "localStartSpeaking",
        vec![
            transmission.voice_client_id.to_string(),
            transmission.language_id.to_string(),
            transmission.speaking_kind.wire_value().to_string(),
            transmission.radio_id.clone(),
        ],
    )
}

fn local_stop_message(transmission: &Transmission) -> Result<AcreMessage, AcreSessionError> {
    message_with_comma(
        "localStopSpeaking",
        vec![
            transmission.voice_client_id.to_string(),
            transmission.speaking_kind.wire_value().to_string(),
            transmission.radio_id.clone(),
        ],
    )
}

fn remote_start_message(transmission: &Transmission) -> Result<AcreMessage, AcreSessionError> {
    message_with_comma(
        "remoteStartSpeaking",
        vec![
            transmission.voice_client_id.to_string(),
            transmission.language_id.to_string(),
            transmission.net_id.clone(),
            transmission.speaking_kind.wire_value().to_string(),
            transmission.radio_id.clone(),
        ],
    )
}

fn remote_stop_message(transmission: &Transmission) -> Result<AcreMessage, AcreSessionError> {
    message_with_comma(
        "remoteStopSpeaking",
        vec![
            transmission.voice_client_id.to_string(),
            transmission.net_id.clone(),
            transmission.speaking_kind.wire_value().to_string(),
            transmission.radio_id.clone(),
        ],
    )
}

fn stop_actions(transmission: &Transmission) -> Result<Vec<AcreAction>, AcreSessionError> {
    Ok(vec![
        AcreAction::SendToArma(local_stop_message(transmission)?),
        AcreAction::LocalTransmissionStopped(transmission.clone()),
    ])
}

/// Validation or state error while translating the ACRE control slice.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum AcreSessionError {
    #[error(transparent)]
    Codec(#[from] AcreCodecError),
    #[error(transparent)]
    Speaking(#[from] AcreSpeakingDataError),
    #[error(transparent)]
    Sound(#[from] AcreSoundError),
    #[error("unsupported ACRE RPC procedure {0}")]
    UnsupportedProcedure(String),
    #[error("ACRE RPC {procedure} has {actual} parameters; expected {expected}")]
    WrongArity {
        procedure: String,
        expected: &'static str,
        actual: usize,
    },
    #[error("a synchronized Mumble local user ID is required")]
    LocalVoiceClientIdUnavailable,
    #[error("a non-empty local ACRE netId is required")]
    LocalNetIdUnavailable,
    #[error("radio PTT requires a non-empty radio ID")]
    EmptyRadioId,
    #[error("ACRE setting name cannot be empty")]
    EmptySettingName,
    #[error("Mumble VOIP metadata is unavailable")]
    VoipMetadataUnavailable,
    #[error("Mumble VOIP metadata field {0} cannot be empty")]
    EmptyVoipMetadataField(&'static str),
    #[error("Mumble VOIP metadata field {field} is {length} bytes; limit is {limit}")]
    VoipMetadataFieldTooLong {
        field: &'static str,
        length: usize,
        limit: usize,
    },
    #[error("Mumble VOIP metadata field {0} cannot be encoded in ACRE RPC")]
    InvalidVoipMetadataField(&'static str),
    #[error("ACRE RPC {procedure} parameter {index} must be a signed integer")]
    InvalidInteger { procedure: String, index: usize },
    #[error("ACRE RPC {procedure} parameter {index} must be a finite float")]
    InvalidFloat { procedure: String, index: usize },
    #[error("ACRE curve scale must be finite")]
    NonFiniteCurveScale,
    #[error("ACRE ping clock must be finite")]
    NonFiniteClock,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpeakingUpdate;

    fn incoming(text: &str) -> AcreMessage {
        AcreMessage::parse(text.as_bytes()).unwrap()
    }

    fn sent(actions: &[AcreAction]) -> Vec<&AcreMessage> {
        actions
            .iter()
            .filter_map(|action| match action {
                AcreAction::SendToArma(message) => Some(message),
                AcreAction::LocalTransmissionStarted(_)
                | AcreAction::LocalTransmissionStopped(_)
                | AcreAction::ListenerUpdated(_)
                | AcreAction::SpeakingUpdated(_)
                | AcreAction::SoundLoaded(_)
                | AcreAction::SoundPlaybackRequested(_)
                | AcreAction::ResetRequested => None,
            })
            .collect()
    }

    fn initialized_local_session() -> AcreSession {
        let mut session = AcreSession::default();
        session.set_local_voice_client_id(Some(41));
        session
            .handle_from_arma(&incoming("getClientID:2:1234,"), 0.0)
            .unwrap();
        session
            .handle_from_arma(&incoming("updateSelf:10,20,30,0,1,0,7,"), 0.0)
            .unwrap();
        session
            .handle_from_arma(&incoming("setSelectableVoiceCurve:0.750000,"), 0.0)
            .unwrap();
        session
    }

    #[test]
    fn handles_health_and_identity_with_stock_acre_wire_shapes() {
        let mut session = AcreSession::default();
        session.set_local_voice_client_id(Some(41));

        let pong = session.handle_from_arma(&incoming("ping:"), 12.5).unwrap();
        assert_eq!(sent(&pong)[0].encode_text(), "pong:12.500000,");

        let version = session
            .handle_from_arma(&incoming("getPluginVersion:"), 0.0)
            .unwrap();
        assert_eq!(
            sent(&version)[0].encode_text(),
            "handleGetPluginVersion:2.14.0.1064"
        );

        let identity = session
            .handle_from_arma(&incoming("getClientID:2:1234,"), 0.0)
            .unwrap();
        assert_eq!(
            sent(&identity)[0].encode_text(),
            "handleGetClientID:41,2:1234,"
        );
        assert_eq!(session.local_net_id(), Some("2:1234"));
    }

    #[test]
    fn emits_a_validated_listener_pose_from_update_self() {
        let mut session = AcreSession::default();
        let actions = session
            .handle_from_arma(&incoming("updateSelf:10,30,20,0,1,0,7,"), 0.0)
            .unwrap();

        assert!(matches!(
            actions.as_slice(),
            [AcreAction::ListenerUpdated(AcreListenerState {
                pose: AcreListenerPose {
                    position: AcreSpeakerVector {
                        x: 10.0,
                        z: 30.0,
                        y: 20.0
                    },
                    head_vector: AcreSpeakerVector {
                        x: 0.0,
                        z: 1.0,
                        y: 0.0
                    },
                },
                curve_model: AcreVoiceCurveModel::Original,
            })]
        ));
    }

    #[test]
    fn curve_model_republishes_an_existing_listener_pose() {
        let mut session = AcreSession::default();
        session
            .handle_from_arma(&incoming("updateSelf:10,30,20,0,1,0,7,"), 0.0)
            .unwrap();

        let actions = session
            .handle_from_arma(&incoming("setVoiceCurveModel:3,0.75,"), 0.0)
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [AcreAction::ListenerUpdated(AcreListenerState {
                curve_model: AcreVoiceCurveModel::SelectableB,
                ..
            })]
        ));
        assert!(matches!(
            session.handle_from_arma(&incoming("setVoiceCurveModel:99,1,"), 0.0),
            Err(AcreSessionError::Speaking(
                AcreSpeakingDataError::InvalidVoiceCurveModel { model: 99 }
            ))
        ));
    }

    #[test]
    fn replies_to_voip_queries_from_a_control_plane_snapshot() {
        let mut session = AcreSession::default();
        session.set_voip_metadata(Some(
            VoipMetadata::new("Mumble", "server-uid", "ACRE Test", "mumble:server-uid:7").unwrap(),
        ));

        let replies = [
            ("getVOIPServerName:", "handleGetVOIPServerName:Mumble"),
            ("getVOIPServerUID:", "handleGetVOIPServerUID:server-uid"),
            ("getVOIPChannelName:", "handleGetVOIPChannelName:ACRE Test"),
            (
                "getVOIPChannelUID:",
                "handleGetVOIPChannelUID:mumble:server-uid:7",
            ),
        ];
        for (request, expected) in replies {
            let actions = session.handle_from_arma(&incoming(request), 0.0).unwrap();
            assert_eq!(sent(&actions)[0].encode_text(), expected);
        }

        session.set_voip_metadata(None);
        assert!(matches!(
            session.handle_from_arma(&incoming("getVOIPServerName:"), 0.0),
            Err(AcreSessionError::VoipMetadataUnavailable)
        ));
    }

    #[test]
    fn rejects_voip_metadata_that_cannot_be_one_acre_parameter() {
        assert!(matches!(
            VoipMetadata::new("Mumble", "server,uid", "channel", "uid"),
            Err(AcreSessionError::InvalidVoipMetadataField("server_uid"))
        ));
        assert!(matches!(
            VoipMetadata::new("Mumble", "server", "", "uid"),
            Err(AcreSessionError::EmptyVoipMetadataField("channel_name"))
        ));
    }

    #[test]
    fn creates_and_closes_a_local_radio_transmission() {
        let mut session = initialized_local_session();

        let start = session
            .handle_from_arma(&incoming("startRadioSpeaking:ACRE_PRC152,"), 0.0)
            .unwrap();
        assert_eq!(
            sent(&start)[0].encode_text(),
            "localStartSpeaking:41,7,1,ACRE_PRC152,"
        );
        assert!(matches!(
            start.last(),
            Some(AcreAction::LocalTransmissionStarted(transmission))
                if transmission.speaking_kind() == SpeakingKind::Radio
        ));

        let stop = session
            .handle_from_arma(&incoming("stopRadioSpeaking:"), 0.0)
            .unwrap();
        assert_eq!(
            sent(&stop)[0].encode_text(),
            "localStopSpeaking:41,1,ACRE_PRC152,"
        );
        assert!(matches!(
            stop.last(),
            Some(AcreAction::LocalTransmissionStopped(_))
        ));
    }

    #[test]
    fn creates_and_transitions_local_god_and_zeus_transmissions() {
        let mut session = initialized_local_session();

        let god = session
            .handle_from_arma(&incoming("startGodModeSpeaking:"), 0.0)
            .unwrap();
        assert_eq!(sent(&god)[0].encode_text(), "localStartSpeaking:41,7,5,,");
        assert!(matches!(
            god.last(),
            Some(AcreAction::LocalTransmissionStarted(transmission))
                if transmission.speaking_kind() == SpeakingKind::God
                    && transmission.radio_id().is_empty()
        ));

        let zeus = session
            .handle_from_arma(&incoming("startZeusSpeaking:"), 0.0)
            .unwrap();
        assert_eq!(
            sent(&zeus)
                .iter()
                .map(|message| message.encode_text())
                .collect::<Vec<_>>(),
            [
                "localStopSpeaking:41,5,,".to_owned(),
                "localStartSpeaking:41,7,6,,".to_owned(),
            ]
        );
        assert!(matches!(
            zeus.last(),
            Some(AcreAction::LocalTransmissionStarted(transmission))
                if transmission.speaking_kind() == SpeakingKind::Zeus
                    && transmission.radio_id().is_empty()
        ));

        let stop = session
            .handle_from_arma(&incoming("stopZeusSpeaking:"), 0.0)
            .unwrap();
        assert_eq!(sent(&stop)[0].encode_text(), "localStopSpeaking:41,6,,");
        assert!(matches!(
            stop.last(),
            Some(AcreAction::LocalTransmissionStopped(transmission))
                if transmission.speaking_kind() == SpeakingKind::Zeus
        ));
    }

    #[test]
    fn remote_transitions_close_the_previous_mode_before_opening_the_next() {
        let mut session = AcreSession::default();
        let radio =
            Transmission::new(99, 0, "2:remote", SpeakingKind::Radio, "ACRE_PRC152", 1.0).unwrap();
        let direct = Transmission::new(99, 0, "2:remote", SpeakingKind::Direct, "", 1.0).unwrap();

        let first = session.remote_started(&radio).unwrap();
        assert_eq!(
            sent(&first)[0].encode_text(),
            "remoteStartSpeaking:99,0,2:remote,1,ACRE_PRC152,"
        );
        let change = session.remote_started(&direct).unwrap();
        assert_eq!(
            sent(&change)
                .iter()
                .map(|message| message.encode_text())
                .collect::<Vec<_>>(),
            [
                "remoteStopSpeaking:99,2:remote,1,ACRE_PRC152,".to_owned(),
                "remoteStartSpeaking:99,0,2:remote,0,,".to_owned(),
            ]
        );
    }

    #[test]
    fn pairs_a_remote_direct_decision_with_the_authenticated_curve_scale() {
        let mut session = AcreSession::default();
        session
            .remote_started(
                &Transmission::new(99, 7, "2:remote", SpeakingKind::Direct, "", 0.75).unwrap(),
            )
            .unwrap();

        let actions = session
            .handle_from_arma(
                &incoming("updateSpeakingData:d,99,0,1,10,30,20,0,1,0,"),
                0.0,
            )
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [AcreAction::SpeakingUpdated(AcreSpeakerAudioUpdate {
                speaking: SpeakingUpdate { speaker_id: 99, .. },
                curve_scale,
            })] if (*curve_scale - 0.75).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn invalid_remote_start_does_not_pin_partial_session_state() {
        let mut session = AcreSession::default();
        let invalid =
            Transmission::new(99, 0, "2:remote,other", SpeakingKind::Direct, "", 1.0).unwrap();

        assert!(matches!(
            session.remote_started(&invalid),
            Err(AcreSessionError::Codec(
                AcreCodecError::InvalidParameter { .. }
            ))
        ));
        assert!(session.remote_stopped(99).unwrap().is_empty());
    }

    #[test]
    fn malformed_messages_do_not_create_partial_state() {
        let mut session = AcreSession::default();
        session.set_local_voice_client_id(Some(41));

        assert_eq!(
            session
                .handle_from_arma(&incoming("getClientID:"), 0.0)
                .unwrap_err(),
            AcreSessionError::WrongArity {
                procedure: "getClientID".to_owned(),
                expected: "one",
                actual: 0,
            }
        );
        assert_eq!(session.local_net_id(), None);
        assert!(matches!(
            session.handle_from_arma(&incoming("startRadioSpeaking:foo,"), 0.0),
            Err(AcreSessionError::LocalNetIdUnavailable)
        ));
        assert!(session.active_local_transmission().is_none());
    }

    #[test]
    fn validates_and_emits_speaking_data_without_turning_it_into_audio_logic() {
        let mut session = AcreSession::default();
        let speaking = session
            .handle_from_arma(&incoming("updateSpeakingData:r,9,0,0,"), 0.0)
            .unwrap();
        assert!(matches!(
            speaking.as_slice(),
            [AcreAction::SpeakingUpdated(AcreSpeakerAudioUpdate {
                speaking: SpeakingUpdate {
                    speaker_id: 9,
                    decision: crate::SpeakingDecision::Radio { paths },
                    ..
                },
                curve_scale,
            })] if paths.is_empty() && (*curve_scale - 1.0).abs() < f32::EPSILON
        ));
        assert!(matches!(
            session.handle_from_arma(&incoming("updateSelf:0,0,0,0,0,0,NaN,"), 0.0),
            Err(AcreSessionError::InvalidInteger { index: 6, .. })
        ));
        assert!(matches!(
            session.handle_from_arma(&incoming("setSelectableVoiceCurve:NaN,"), 0.0),
            Err(AcreSessionError::InvalidFloat { index: 0, .. })
        ));
    }

    #[test]
    fn emits_completed_loaded_sounds_and_typed_play_requests_without_audio_work() {
        let mut session = AcreSession::default();
        assert!(
            session
                .handle_from_arma(&incoming("loadSound:ACRE_Click,1,3,QUJD,"), 0.0)
                .unwrap()
                .is_empty()
        );
        assert!(
            session
                .handle_from_arma(&incoming("loadSound:ACRE_Click,2,3,REVG,"), 0.0)
                .unwrap()
                .is_empty()
        );
        let loaded = session
            .handle_from_arma(&incoming("loadSound:ACRE_Click,3,3,,"), 0.0)
            .unwrap();
        assert!(matches!(
            loaded.as_slice(),
            [AcreAction::SoundLoaded(sound)] if sound.id() == "ACRE_Click" && sound.bytes() == b"ABCDEF"
        ));

        let playback = session
            .handle_from_arma(
                &incoming("playLoadedSound:ACRE_Click,0,0,0,0,0,1,0.5,0,"),
                0.0,
            )
            .unwrap();
        assert!(matches!(
            playback.as_slice(),
            [AcreAction::SoundPlaybackRequested(request)]
                if request.id() == "ACRE_Click"
                    && !request.is_world()
                    && (request.volume() - 0.5).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn reset_clears_local_and_remote_state() {
        let mut session = AcreSession::default();
        session.set_local_voice_client_id(Some(41));
        session
            .handle_from_arma(&incoming("getClientID:2:local,"), 0.0)
            .unwrap();
        session
            .handle_from_arma(&incoming("startIntercomSpeaking:"), 0.0)
            .unwrap();
        session
            .remote_started(
                &Transmission::new(99, 0, "2:remote", SpeakingKind::Direct, "", 1.0).unwrap(),
            )
            .unwrap();

        assert_eq!(
            session.handle_from_arma(&incoming("ext_reset:41,"), 0.0),
            Ok(vec![AcreAction::ResetRequested])
        );
        assert!(session.active_local_transmission().is_none());
        assert!(session.remote_stopped(99).unwrap().is_empty());
    }
}
