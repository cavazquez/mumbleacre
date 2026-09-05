//! Allocation-free planning for ACRE radio reception paths.
//!
//! ACRE already decides which radio paths are audible and supplies each path's
//! volume, signal quality, signal model, loudspeaker flag, and position. This
//! module deliberately preserves those decisions rather than reconsidering
//! frequency, range, radio ownership, or propagation. The AM-13 renderer owns
//! the per-path DSP state and PCM accumulation; this planner is its bounded,
//! pure boundary.

use std::time::Instant;

use mumbleacre_acre::{
    AcreAudioSnapshot, AcreListenerPose, AcreSpeakerVector, RadioReceptionPath, SpeakingDecision,
};

use crate::acre_direct::ACRE_AUDIO_STATE_TTL;

const MIN_AUDIBLE_VOLUME: f32 = 0.001;
const RADIO_FULL_VOLUME_M: f32 = 8.0;
const RADIO_CUTOFF_M: f32 = 150.0;
const INNER_CONE_HALF_ANGLE_RAD: f32 = std::f32::consts::FRAC_PI_8;
const OUTER_CONE_HALF_ANGLE_RAD: f32 = std::f32::consts::FRAC_PI_4;
const INNER_CONE_VOLUME: f32 = 1.2;
const OUTER_CONE_VOLUME: f32 = 1.0;

/// One radio path ready for a future DSP/mix stage. `signal_model` stays
/// opaque because the pinned ACRE2 core accepts it as a parameter but does not
/// reinterpret radio reachability in the voice backend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AcreRadioPathPlan {
    pub(crate) path_index: usize,
    pub(crate) volume: f32,
    pub(crate) signal_quality: f32,
    pub(crate) signal_model: f32,
    pub(crate) loudspeaker: bool,
    pub(crate) position: AcreSpeakerVector,
    /// Spatial gain before the path's ACRE volume envelope.  The renderer
    /// needs this separate value to reproduce the native order: radio DSP,
    /// volume ramp, then positional mixdown.
    pub(crate) spatial_mono_gain: f32,
    pub(crate) spatial_left_gain: f32,
    pub(crate) spatial_right_gain: f32,
    pub(crate) mono_gain: f32,
    pub(crate) left_gain: f32,
    pub(crate) right_gain: f32,
}

impl AcreRadioPathPlan {
    pub(crate) const SILENT: Self = Self {
        path_index: 0,
        volume: 0.0,
        signal_quality: 0.0,
        signal_model: 0.0,
        loudspeaker: false,
        position: AcreSpeakerVector {
            x: 0.0,
            z: 0.0,
            y: 0.0,
        },
        spatial_mono_gain: 0.0,
        spatial_left_gain: 0.0,
        spatial_right_gain: 0.0,
        mono_gain: 0.0,
        left_gain: 0.0,
        right_gain: 0.0,
    };
}

/// Shared positional mixdown gains for ACRE modes that use the stock radio
/// curve. ACRE applies that curve to both radio reception paths and intercom;
/// keeping it here prevents the two callback renderers from subtly drifting
/// apart while still leaving radio reachability decisions in SQF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AcreRadioSpatialGains {
    pub(crate) distance_m: f32,
    pub(crate) mono_gain: f32,
    pub(crate) left_gain: f32,
    pub(crate) right_gain: f32,
}

/// Calculates only the X3DAudio-style spatial stage shared by ACRE radio and
/// intercom. Callers retain ownership of the source's ACRE-decided volume and
/// any radio DSP configuration, so this function cannot reintroduce legacy
/// frequency, vehicle, or channel policy.
pub(crate) fn radio_spatial_gains(
    listener: AcreListenerPose,
    speaker_position: AcreSpeakerVector,
    speaker_head: AcreSpeakerVector,
) -> Option<AcreRadioSpatialGains> {
    let distance_m = distance(listener.position, speaker_position)?;
    let distance_gain = radio_distance_gain(distance_m);
    let cone_gain = speaker_cone_gain(speaker_head, speaker_position, listener.position);
    let mono_gain = distance_gain * cone_gain;
    if !mono_gain.is_finite() || mono_gain <= MIN_AUDIBLE_VOLUME {
        return None;
    }
    let (left_gain, right_gain) = stereo_gains(mono_gain, listener, speaker_position);
    Some(AcreRadioSpatialGains {
        distance_m,
        mono_gain,
        left_gain,
        right_gain,
    })
}

/// Writes every valid ACRE radio path into `output` in the exact incoming
/// order. `Some(0)` means a valid radio decision with no audible path; `None`
/// means the speaker is missing, stale, non-radio, malformed, or exceeds the
/// caller's already preallocated capacity.
pub(crate) fn plan_radio_paths(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    now: Instant,
    output: &mut [AcreRadioPathPlan],
) -> Option<usize> {
    let listener = snapshot.listener()?;
    let speaker = snapshot.speaker(speaker_id)?;
    if !listener.is_fresh(ACRE_AUDIO_STATE_TTL, now) || !speaker.is_fresh(ACRE_AUDIO_STATE_TTL, now)
    {
        return None;
    }
    let SpeakingDecision::Radio { paths } = speaker.decision() else {
        return None;
    };
    if paths.len() > output.len() {
        return None;
    }

    let mut count = 0;
    for (path_index, path) in paths.iter().enumerate() {
        let Some(plan) = radio_path_plan(listener.pose(), path, path_index) else {
            continue;
        };
        output[count] = plan;
        count += 1;
    }
    Some(count)
}

fn radio_path_plan(
    world_listener: AcreListenerPose,
    path: &RadioReceptionPath,
    path_index: usize,
) -> Option<AcreRadioPathPlan> {
    if !path.volume.is_finite()
        || path.volume <= MIN_AUDIBLE_VOLUME
        || !(0.0..=1.0).contains(&path.signal_quality)
        || !path.signal_model.is_finite()
    {
        return None;
    }

    // The ACRE2 core evaluates loudspeakers in world space. Headset paths are
    // already supplied as local offsets (for example x=-2/0/2 for left/center/
    // right), so their listener remains at the local origin.
    let listener = if path.loudspeaker {
        world_listener
    } else {
        AcreListenerPose {
            position: zero_vector(),
            head_vector: forward_vector(),
        }
    };
    let spatial = radio_spatial_gains(listener, path.position, forward_vector())?;
    let mono_gain = (path.volume * spatial.mono_gain).clamp(0.0, 1.0);
    let left_gain = (path.volume * spatial.left_gain).clamp(0.0, 1.0);
    let right_gain = (path.volume * spatial.right_gain).clamp(0.0, 1.0);
    if mono_gain <= MIN_AUDIBLE_VOLUME {
        return None;
    }
    Some(AcreRadioPathPlan {
        path_index,
        volume: path.volume,
        signal_quality: path.signal_quality,
        signal_model: path.signal_model,
        loudspeaker: path.loudspeaker,
        position: path.position,
        spatial_mono_gain: spatial.mono_gain,
        spatial_left_gain: spatial.left_gain,
        spatial_right_gain: spatial.right_gain,
        mono_gain,
        left_gain,
        right_gain,
    })
}

/// Radio paths use the stock ACRE/X3DAudio curve: flat through 8m, then
/// `(25 / (3 * distance))²`, with a hard 150m cutoff.
fn radio_distance_gain(distance_m: f32) -> f32 {
    if distance_m <= RADIO_FULL_VOLUME_M {
        1.0
    } else if distance_m >= RADIO_CUTOFF_M || !distance_m.is_finite() {
        0.0
    } else {
        let gain = 25.0 / (3.0 * distance_m);
        gain * gain
    }
}

fn speaker_cone_gain(
    speaker_head: AcreSpeakerVector,
    speaker_position: AcreSpeakerVector,
    listener_position: AcreSpeakerVector,
) -> f32 {
    let Some(to_listener) = normalized(vector_between(speaker_position, listener_position)) else {
        return INNER_CONE_VOLUME;
    };
    let Some(speaker_forward) = normalized(speaker_head) else {
        return OUTER_CONE_VOLUME;
    };
    let dot = dot(speaker_forward, to_listener).clamp(-1.0, 1.0);
    let inner = INNER_CONE_HALF_ANGLE_RAD.cos();
    let outer = OUTER_CONE_HALF_ANGLE_RAD.cos();
    if dot >= inner {
        INNER_CONE_VOLUME
    } else if dot <= outer {
        OUTER_CONE_VOLUME
    } else {
        let t = (dot - outer) / (inner - outer);
        OUTER_CONE_VOLUME + ((INNER_CONE_VOLUME - OUTER_CONE_VOLUME) * t)
    }
}

fn stereo_gains(
    mono_gain: f32,
    listener: AcreListenerPose,
    speaker_position: AcreSpeakerVector,
) -> (f32, f32) {
    let relative = vector_between(listener.position, speaker_position);
    let horizontal_length = (relative.x.mul_add(relative.x, relative.z * relative.z)).sqrt();
    let forward_length = (listener.head_vector.x.mul_add(
        listener.head_vector.x,
        listener.head_vector.z * listener.head_vector.z,
    ))
    .sqrt();
    if !horizontal_length.is_finite()
        || !forward_length.is_finite()
        || horizontal_length <= f32::MIN_POSITIVE
        || forward_length <= f32::MIN_POSITIVE
    {
        return (mono_gain, mono_gain);
    }

    let right_x = listener.head_vector.z / forward_length;
    let right_z = -listener.head_vector.x / forward_length;
    let pan = ((relative.x * right_x) + (relative.z * right_z)) / horizontal_length;
    let pan = pan.clamp(-1.0, 1.0);
    (
        mono_gain * ((1.0 - pan) * 0.5).sqrt(),
        mono_gain * ((1.0 + pan) * 0.5).sqrt(),
    )
}

fn distance(left: AcreSpeakerVector, right: AcreSpeakerVector) -> Option<f32> {
    let delta = vector_between(left, right);
    let distance = delta
        .x
        .mul_add(delta.x, delta.y.mul_add(delta.y, delta.z * delta.z))
        .sqrt();
    distance.is_finite().then_some(distance)
}

fn vector_between(from: AcreSpeakerVector, to: AcreSpeakerVector) -> AcreSpeakerVector {
    AcreSpeakerVector {
        x: to.x - from.x,
        z: to.z - from.z,
        y: to.y - from.y,
    }
}

fn normalized(vector: AcreSpeakerVector) -> Option<AcreSpeakerVector> {
    let length = vector
        .x
        .mul_add(vector.x, vector.y.mul_add(vector.y, vector.z * vector.z))
        .sqrt();
    if !length.is_finite() || length <= f32::MIN_POSITIVE {
        return None;
    }
    Some(AcreSpeakerVector {
        x: vector.x / length,
        z: vector.z / length,
        y: vector.y / length,
    })
}

fn dot(left: AcreSpeakerVector, right: AcreSpeakerVector) -> f32 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

const fn zero_vector() -> AcreSpeakerVector {
    AcreSpeakerVector {
        x: 0.0,
        z: 0.0,
        y: 0.0,
    }
}

const fn forward_vector() -> AcreSpeakerVector {
    AcreSpeakerVector {
        x: 0.0,
        z: 1.0,
        y: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{
        ACRE_MAX_RADIO_RECEPTION_PATHS, AcreAudioSnapshotBuilder, AcreAudioUpdate,
        AcreListenerState, AcreVoiceCurveModel, SpatialSpeakingKind, SpeakingUpdate,
    };

    fn vector(x: f32, y: f32, z: f32) -> AcreSpeakerVector {
        AcreSpeakerVector { x, z, y }
    }

    fn snapshot(paths: Vec<RadioReceptionPath>, at: Instant) -> AcreAudioSnapshot {
        let mut builder = AcreAudioSnapshotBuilder::default();
        builder
            .apply_update(
                AcreAudioUpdate::Listener(AcreListenerState {
                    pose: AcreListenerPose {
                        position: vector(10.0, 0.0, 0.0),
                        head_vector: vector(0.0, 0.0, 1.0),
                    },
                    curve_model: AcreVoiceCurveModel::Original,
                }),
                at,
            )
            .unwrap();
        builder
            .apply_update(
                AcreAudioUpdate::Speaker(
                    SpeakingUpdate {
                        speaker_id: 42,
                        speaks_babel: false,
                        decision: SpeakingDecision::Radio { paths },
                    }
                    .into(),
                ),
                at,
            )
            .unwrap()
    }

    fn output() -> [AcreRadioPathPlan; ACRE_MAX_RADIO_RECEPTION_PATHS] {
        [AcreRadioPathPlan::SILENT; ACRE_MAX_RADIO_RECEPTION_PATHS]
    }

    #[test]
    fn preserves_all_valid_paths_without_recalculating_radio_routing() {
        let now = Instant::now();
        let snapshot = snapshot(
            vec![
                RadioReceptionPath {
                    volume: 0.5,
                    signal_quality: 0.8,
                    signal_model: 2.0,
                    loudspeaker: false,
                    position: vector(-2.0, 0.0, 0.0),
                },
                RadioReceptionPath {
                    volume: 0.8,
                    signal_quality: 0.6,
                    signal_model: 3.0,
                    loudspeaker: true,
                    position: vector(14.0, 0.0, 0.0),
                },
            ],
            now,
        );
        let mut paths = output();
        let count = plan_radio_paths(&snapshot, 42, now, &mut paths).unwrap();

        assert_eq!(count, 2);
        assert_eq!(paths[0].path_index, 0);
        assert!(!paths[0].loudspeaker);
        assert!((paths[0].signal_quality - 0.8).abs() < f32::EPSILON);
        assert!((paths[0].signal_model - 2.0).abs() < f32::EPSILON);
        assert!(paths[0].left_gain > paths[0].right_gain);
        assert_eq!(paths[1].path_index, 1);
        assert!(paths[1].loudspeaker);
        assert!((paths[1].volume - 0.8).abs() < f32::EPSILON);
        assert!((paths[1].signal_quality - 0.6).abs() < f32::EPSILON);
        assert!((paths[1].signal_model - 3.0).abs() < f32::EPSILON);
        assert!(paths[1].right_gain > paths[1].left_gain);
    }

    #[test]
    fn represents_a_valid_empty_radio_decision_as_silence() {
        let now = Instant::now();
        let snapshot = snapshot(Vec::new(), now);
        let mut paths = output();
        assert_eq!(plan_radio_paths(&snapshot, 42, now, &mut paths), Some(0));
    }

    #[test]
    fn fails_closed_for_stale_non_radio_or_out_of_curve_paths() {
        let now = Instant::now();
        let snapshot = snapshot(
            vec![RadioReceptionPath {
                volume: 1.0,
                signal_quality: 1.0,
                signal_model: 0.0,
                loudspeaker: true,
                position: vector(160.0, 0.0, 0.0),
            }],
            now,
        );
        let mut paths = output();
        assert_eq!(plan_radio_paths(&snapshot, 42, now, &mut paths), Some(0));
        assert_eq!(
            plan_radio_paths(
                &snapshot,
                42,
                now + ACRE_AUDIO_STATE_TTL + std::time::Duration::from_millis(1),
                &mut paths,
            ),
            None
        );

        let mut builder = AcreAudioSnapshotBuilder::default();
        builder
            .apply_update(
                AcreAudioUpdate::Listener(AcreListenerState {
                    pose: AcreListenerPose {
                        position: zero_vector(),
                        head_vector: forward_vector(),
                    },
                    curve_model: AcreVoiceCurveModel::Original,
                }),
                now,
            )
            .unwrap();
        let direct = builder
            .apply_update(
                AcreAudioUpdate::Speaker(
                    SpeakingUpdate {
                        speaker_id: 42,
                        speaks_babel: false,
                        decision: SpeakingDecision::Spatial {
                            kind: SpatialSpeakingKind::Direct,
                            volume: 1.0,
                            position: zero_vector(),
                            head_vector: forward_vector(),
                        },
                    }
                    .into(),
                ),
                now,
            )
            .unwrap();
        assert_eq!(plan_radio_paths(&direct, 42, now, &mut paths), None);
    }
}
