//! Allocation-free direct-voice renderer for the ACRE audio snapshot.
//!
//! The implementation mirrors the relevant `ACRE2Core` v2.14 spatial inputs:
//! the packet's volume effect is separate from distance, speaker cone, and
//! listener-relative stereo matrix. It handles ACRE's `d` and `z` decisions;
//! radio multipath and intercom have their own renderers. Babel is applied
//! before direct gain/pan. Zeus shares ACRE's direct-like spatial decision;
//! spectator and God use their dedicated non-spatial renderer.

use std::time::{Duration, Instant};

use mumbleacre_acre::{
    AcreAudioSnapshot, AcreListenerPose, AcreVoiceCurveModel, SpatialSpeakingKind, SpeakingDecision,
};

use crate::acre_babel::AcreBabelState;

/// Conservative freshness window until AM-18 centralizes all ACRE lifecycle
/// timeouts. It matches the existing control-state timeout and causes a stale
/// decision to mute rather than fall back to legacy positional audio.
pub(crate) const ACRE_AUDIO_STATE_TTL: Duration = Duration::from_secs(2);

const MIN_AUDIBLE_VOLUME: f32 = 0.001;
const SELECTABLE_B_FULL_VOLUME_M: f32 = 8.0;
const SELECTABLE_B_CUTOFF_M: f32 = 150.0;
const AMPLITUDE_KILL_START_M: f32 = 75.0;
const AMPLITUDE_KILL_RANGE_M: f32 = 150.0;
const INNER_CONE_HALF_ANGLE_RAD: f32 = std::f32::consts::FRAC_PI_8;
const OUTER_CONE_HALF_ANGLE_RAD: f32 = std::f32::consts::FRAC_PI_4;
const INNER_CONE_VOLUME: f32 = 1.2;
const OUTER_CONE_VOLUME: f32 = 1.0;

/// Precomputed gains for one direct ACRE decision. It keeps the geometry pure
/// and testable before any samples are mutated on the Mumble callback thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AcreDirectPlan {
    pub(crate) distance_m: f32,
    pub(crate) speaks_babel: bool,
    pub(crate) mono_gain: f32,
    pub(crate) left_gain: f32,
    pub(crate) right_gain: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcreDirectRenderAction {
    Modified,
    Mute,
}

/// Persistent per-speaker gain state for ACRE's volume interpolation. The
/// control plane creates one state per accepted speaker; applying it mutates
/// only scalar fields on the audio callback thread.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcreDirectGainState {
    left_gain: f32,
    right_gain: f32,
    initialized: bool,
    babel: [AcreBabelState; 2],
}

impl Default for AcreDirectGainState {
    fn default() -> Self {
        Self {
            left_gain: 0.0,
            right_gain: 0.0,
            initialized: false,
            babel: [AcreBabelState::new(), AcreBabelState::new()],
        }
    }
}

impl AcreDirectGainState {
    pub(crate) fn reset(&mut self) {
        self.left_gain = 0.0;
        self.right_gain = 0.0;
        self.initialized = false;
        for babel in &mut self.babel {
            babel.reset();
        }
    }

    fn apply(
        &mut self,
        samples: &mut [f32],
        channel_count: usize,
        sample_rate: u32,
        plan: AcreDirectPlan,
    ) -> bool {
        if !self.apply_babel(samples, channel_count, sample_rate, plan.speaks_babel) {
            return false;
        }
        let (start_left, start_right) = if self.initialized {
            (self.left_gain, self.right_gain)
        } else {
            (0.0, 0.0)
        };
        match channel_count {
            1 => apply_mono_ramp(samples, start_left, plan.mono_gain),
            2 => apply_stereo_ramp(
                samples,
                start_left,
                plan.left_gain,
                start_right,
                plan.right_gain,
            ),
            _ => unreachable!("channel count validated before applying ACRE gains"),
        }
        self.left_gain = if channel_count == 1 {
            plan.mono_gain
        } else {
            plan.left_gain
        };
        self.right_gain = if channel_count == 1 {
            plan.mono_gain
        } else {
            plan.right_gain
        };
        self.initialized = true;
        true
    }

    fn apply_babel(
        &mut self,
        samples: &mut [f32],
        channel_count: usize,
        sample_rate: u32,
        speaks_babel: bool,
    ) -> bool {
        if channel_count == 1 {
            // Mumble normally keeps a source format stable, but a format
            // transition must not resurrect the unused stereo channel's
            // filter history when it becomes active again.
            self.babel[1].reset();
        }
        for state in &mut self.babel[..channel_count] {
            if !state.prepare_frame(sample_rate, speaks_babel) {
                return false;
            }
        }
        match channel_count {
            1 => {
                for sample in samples {
                    *sample = self.babel[0].process_sample(*sample);
                }
            }
            2 => {
                for frame in samples.chunks_exact_mut(2) {
                    frame[0] = self.babel[0].process_sample(frame[0]);
                    frame[1] = self.babel[1].process_sample(frame[1]);
                }
            }
            _ => unreachable!("channel count validated before applying ACRE Babel"),
        }
        true
    }
}

/// Builds a direct-like (`d` or `z`) render plan for `speaker_id`. A missing,
/// stale, mute, unsupported, or malformed state produces `None`, which the
/// caller maps to silence. No legacy state is consulted here.
pub(crate) fn direct_plan(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    now: Instant,
) -> Option<AcreDirectPlan> {
    let listener = snapshot.listener()?;
    let speaker = snapshot.speaker(speaker_id)?;
    if !listener.is_fresh(ACRE_AUDIO_STATE_TTL, now) || !speaker.is_fresh(ACRE_AUDIO_STATE_TTL, now)
    {
        return None;
    }
    let SpeakingDecision::Spatial {
        kind,
        volume,
        position,
        head_vector,
    } = speaker.decision()
    else {
        return None;
    };
    if !matches!(
        kind,
        SpatialSpeakingKind::Direct | SpatialSpeakingKind::Zeus
    ) {
        return None;
    }
    if !volume.is_finite() || *volume <= MIN_AUDIBLE_VOLUME {
        return None;
    }

    let listener_pose = listener.pose();
    let distance_m = distance(listener_pose.position, *position)?;
    let distance_gain =
        direct_distance_gain(listener.curve_model(), speaker.curve_scale(), distance_m)?;
    let cone_gain = speaker_cone_gain(*head_vector, *position, listener_pose.position);
    let mono_gain = (*volume * distance_gain * cone_gain).clamp(0.0, 1.0);
    if !mono_gain.is_finite() || mono_gain <= MIN_AUDIBLE_VOLUME {
        return None;
    }
    let (left_gain, right_gain) = stereo_gains(mono_gain, listener_pose, *position);
    Some(AcreDirectPlan {
        distance_m,
        speaks_babel: speaker.speaks_babel(),
        mono_gain,
        left_gain,
        right_gain,
    })
}

/// Applies the plan in-place without allocating, taking a mutex, I/O, or a
/// Mumble API. Non-finite input samples are zeroed to retain a safe callback
/// output even when an upstream decoder misbehaves.
#[cfg(test)]
pub(crate) fn render_direct(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    samples: &mut [f32],
    channel_count: usize,
    now: Instant,
) -> AcreDirectRenderAction {
    let mut gain_state = AcreDirectGainState::default();
    render_direct_with_gain_state(
        snapshot,
        speaker_id,
        samples,
        channel_count,
        48_000,
        now,
        &mut gain_state,
    )
}

/// Preserves ACRE's frame-to-frame volume interpolation for one speaker. The
/// caller must have prepared `gain_state` off the callback thread; a missing
/// state is therefore a fail-closed error at the RT hub rather than a reason to
/// allocate here.
pub(crate) fn render_direct_with_gain_state(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    samples: &mut [f32],
    channel_count: usize,
    sample_rate: u32,
    now: Instant,
    gain_state: &mut AcreDirectGainState,
) -> AcreDirectRenderAction {
    if samples.is_empty()
        || !(1..=2).contains(&channel_count)
        || !samples.len().is_multiple_of(channel_count)
        || samples.len() > usize::from(u16::MAX)
    {
        gain_state.reset();
        return AcreDirectRenderAction::Mute;
    }
    let Some(plan) = direct_plan(snapshot, speaker_id, now) else {
        gain_state.reset();
        return AcreDirectRenderAction::Mute;
    };
    if gain_state.apply(samples, channel_count, sample_rate, plan) {
        AcreDirectRenderAction::Modified
    } else {
        gain_state.reset();
        AcreDirectRenderAction::Mute
    }
}

fn direct_distance_gain(
    model: AcreVoiceCurveModel,
    curve_scale: f32,
    distance_m: f32,
) -> Option<f32> {
    if !curve_scale.is_finite() || curve_scale <= f32::MIN_POSITIVE || !distance_m.is_finite() {
        return None;
    }
    let gain = match model {
        // X3DAudio's NULL `pVolumeCurve`: no attenuation up to the scaler,
        // then `CurveDistanceScaler / distance`.
        AcreVoiceCurveModel::Original => x3d_default_gain(distance_m, 1.0),
        AcreVoiceCurveModel::Amplitude => {
            x3d_default_gain(distance_m, 1.0) * amplitude_kill_gain(distance_m)
        }
        // ACRE's mixer applies the same 75–225 m kill coefficient to both
        // amplitude and selectable-A; selectable-A additionally uses the
        // remote speaker's curve scale for the X3D attenuation.
        AcreVoiceCurveModel::SelectableA => {
            x3d_default_gain(distance_m, curve_scale) * amplitude_kill_gain(distance_m)
        }
        AcreVoiceCurveModel::SelectableB => selectable_b_gain(distance_m / curve_scale),
    };
    gain.is_finite().then_some(gain.clamp(0.0, 1.0))
}

fn x3d_default_gain(distance_m: f32, curve_scale: f32) -> f32 {
    if distance_m <= curve_scale {
        1.0
    } else {
        curve_scale / distance_m
    }
}

fn amplitude_kill_gain(distance_m: f32) -> f32 {
    if distance_m <= AMPLITUDE_KILL_START_M {
        1.0
    } else {
        (1.0 - ((distance_m - AMPLITUDE_KILL_START_M) / AMPLITUDE_KILL_RANGE_M)).clamp(0.0, 1.0)
    }
}

/// The stock selectable-B table is flat through 8m, follows
/// `(25 / (3 * distance))²`, then cuts off at 150m.
fn selectable_b_gain(distance_m: f32) -> f32 {
    if distance_m <= SELECTABLE_B_FULL_VOLUME_M {
        1.0
    } else if distance_m >= SELECTABLE_B_CUTOFF_M {
        0.0
    } else {
        let gain = 25.0 / (3.0 * distance_m);
        gain * gain
    }
}

fn speaker_cone_gain(
    speaker_head: mumbleacre_acre::AcreSpeakerVector,
    speaker_position: mumbleacre_acre::AcreSpeakerVector,
    listener_position: mumbleacre_acre::AcreSpeakerVector,
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
    speaker_position: mumbleacre_acre::AcreSpeakerVector,
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

    // DirectX/ACRE use x=right, y=up, z=forward. `right` is `up × forward`.
    let right_x = listener.head_vector.z / forward_length;
    let right_z = -listener.head_vector.x / forward_length;
    let pan = ((relative.x * right_x) + (relative.z * right_z)) / horizontal_length;
    let pan = pan.clamp(-1.0, 1.0);
    (
        mono_gain * ((1.0 - pan) * 0.5).sqrt(),
        mono_gain * ((1.0 + pan) * 0.5).sqrt(),
    )
}

fn apply_mono_ramp(samples: &mut [f32], previous_gain: f32, target_gain: f32) {
    let sample_count = u16::try_from(samples.len())
        .expect("ACRE renderer rejects a buffer longer than u16::MAX samples");
    let step = 1.0 / f32::from(sample_count);
    let mut progress = 0.0;
    for sample in samples {
        let gain = previous_gain + ((target_gain - previous_gain) * progress);
        *sample = scaled_sample(*sample, gain);
        progress += step;
    }
}

fn apply_stereo_ramp(
    samples: &mut [f32],
    previous_left: f32,
    target_left: f32,
    previous_right: f32,
    target_right: f32,
) {
    let frame_count = u16::try_from(samples.len() / 2)
        .expect("ACRE renderer rejects a buffer longer than u16::MAX samples");
    let step = 1.0 / f32::from(frame_count);
    let mut progress = 0.0;
    for frame in samples.chunks_exact_mut(2) {
        let left_gain = previous_left + ((target_left - previous_left) * progress);
        let right_gain = previous_right + ((target_right - previous_right) * progress);
        frame[0] = scaled_sample(frame[0], left_gain);
        frame[1] = scaled_sample(frame[1], right_gain);
        progress += step;
    }
}

fn scaled_sample(sample: f32, gain: f32) -> f32 {
    let scaled = sample * gain;
    if scaled.is_finite() {
        scaled.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn distance(
    left: mumbleacre_acre::AcreSpeakerVector,
    right: mumbleacre_acre::AcreSpeakerVector,
) -> Option<f32> {
    let delta = vector_between(left, right);
    let distance = delta
        .x
        .mul_add(delta.x, delta.y.mul_add(delta.y, delta.z * delta.z))
        .sqrt();
    distance.is_finite().then_some(distance)
}

fn vector_between(
    from: mumbleacre_acre::AcreSpeakerVector,
    to: mumbleacre_acre::AcreSpeakerVector,
) -> mumbleacre_acre::AcreSpeakerVector {
    mumbleacre_acre::AcreSpeakerVector {
        x: to.x - from.x,
        z: to.z - from.z,
        y: to.y - from.y,
    }
}

fn normalized(
    vector: mumbleacre_acre::AcreSpeakerVector,
) -> Option<mumbleacre_acre::AcreSpeakerVector> {
    let length = vector
        .x
        .mul_add(vector.x, vector.y.mul_add(vector.y, vector.z * vector.z))
        .sqrt();
    if !length.is_finite() || length <= f32::MIN_POSITIVE {
        return None;
    }
    Some(mumbleacre_acre::AcreSpeakerVector {
        x: vector.x / length,
        z: vector.z / length,
        y: vector.y / length,
    })
}

fn dot(left: mumbleacre_acre::AcreSpeakerVector, right: mumbleacre_acre::AcreSpeakerVector) -> f32 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{
        AcreAudioSnapshotBuilder, AcreAudioUpdate, AcreListenerState, AcreSpeakerAudioUpdate,
        AcreSpeakerVector, SpeakingUpdate,
    };

    fn vector(x: f32, y: f32, z: f32) -> AcreSpeakerVector {
        AcreSpeakerVector { x, z, y }
    }

    fn snapshot(
        model: AcreVoiceCurveModel,
        curve_scale: f32,
        speaker_position: AcreSpeakerVector,
        speaker_head: AcreSpeakerVector,
    ) -> AcreAudioSnapshot {
        snapshot_with_babel(model, curve_scale, speaker_position, speaker_head, false)
    }

    fn snapshot_with_babel(
        model: AcreVoiceCurveModel,
        curve_scale: f32,
        speaker_position: AcreSpeakerVector,
        speaker_head: AcreSpeakerVector,
        speaks_babel: bool,
    ) -> AcreAudioSnapshot {
        snapshot_with_kind(
            model,
            curve_scale,
            speaker_position,
            speaker_head,
            SpatialSpeakingKind::Direct,
            speaks_babel,
        )
    }

    fn snapshot_with_kind(
        model: AcreVoiceCurveModel,
        curve_scale: f32,
        speaker_position: AcreSpeakerVector,
        speaker_head: AcreSpeakerVector,
        kind: SpatialSpeakingKind,
        speaks_babel: bool,
    ) -> AcreAudioSnapshot {
        let now = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::default();
        builder
            .apply_update(
                AcreAudioUpdate::Listener(AcreListenerState {
                    pose: AcreListenerPose {
                        position: vector(0.0, 0.0, 0.0),
                        head_vector: vector(0.0, 0.0, 1.0),
                    },
                    curve_model: model,
                }),
                now,
            )
            .unwrap();
        builder
            .apply_update(
                AcreAudioUpdate::Speaker(
                    AcreSpeakerAudioUpdate::new(
                        SpeakingUpdate {
                            speaker_id: 42,
                            speaks_babel,
                            decision: SpeakingDecision::Spatial {
                                kind,
                                volume: 1.0,
                                position: speaker_position,
                                head_vector: speaker_head,
                            },
                        },
                        curve_scale,
                    )
                    .unwrap(),
                ),
                now,
            )
            .unwrap()
    }

    #[test]
    fn applies_acre_direct_distance_curves_without_legacy_range_logic() {
        let now = Instant::now();
        let forward = vector(0.0, 0.0, 0.0);
        let original = snapshot(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(0.0, 0.0, 2.0),
            forward,
        );
        let selectable_a = snapshot(
            AcreVoiceCurveModel::SelectableA,
            2.0,
            vector(0.0, 0.0, 4.0),
            forward,
        );
        let selectable_a_falloff = snapshot(
            AcreVoiceCurveModel::SelectableA,
            2.0,
            vector(0.0, 0.0, 100.0),
            forward,
        );
        let selectable_b = snapshot(
            AcreVoiceCurveModel::SelectableB,
            1.0,
            vector(0.0, 0.0, 10.0),
            forward,
        );
        let amplitude = snapshot(
            AcreVoiceCurveModel::Amplitude,
            1.0,
            vector(0.0, 0.0, 100.0),
            forward,
        );

        assert!((direct_plan(&original, 42, now).unwrap().mono_gain - 0.5).abs() < 1e-6);
        assert!((direct_plan(&selectable_a, 42, now).unwrap().mono_gain - 0.5).abs() < 1e-6);
        assert!(
            (direct_plan(&selectable_a_falloff, 42, now)
                .unwrap()
                .mono_gain
                - (1.0 / 60.0))
                .abs()
                < 1e-6
        );
        assert!(
            (direct_plan(&selectable_b, 42, now).unwrap().mono_gain - (25.0_f32 / 30.0).powi(2))
                .abs()
                < 1e-6
        );
        assert!((direct_plan(&amplitude, 42, now).unwrap().mono_gain - (1.0 / 120.0)).abs() < 1e-6);
    }

    #[test]
    fn uses_listener_and_speaker_direction_for_stereo_pan_and_cone() {
        let now = Instant::now();
        let right = snapshot(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(2.0, 0.0, 0.0),
            vector(-1.0, 0.0, 0.0),
        );
        let plan = direct_plan(&right, 42, now).unwrap();

        assert!(plan.right_gain > plan.left_gain);
        assert!(plan.mono_gain > 0.5, "the speaker faces the listener");
    }

    #[test]
    fn zeus_uses_the_same_acre_spatial_inputs_as_direct_voice() {
        let now = Instant::now();
        let direct = snapshot_with_kind(
            AcreVoiceCurveModel::SelectableB,
            0.8,
            vector(3.0, 0.0, 12.0),
            vector(-1.0, 0.0, 0.0),
            SpatialSpeakingKind::Direct,
            false,
        );
        let zeus = snapshot_with_kind(
            AcreVoiceCurveModel::SelectableB,
            0.8,
            vector(3.0, 0.0, 12.0),
            vector(-1.0, 0.0, 0.0),
            SpatialSpeakingKind::Zeus,
            false,
        );

        assert_eq!(direct_plan(&zeus, 42, now), direct_plan(&direct, 42, now));
    }

    #[test]
    fn fails_closed_for_stale_or_non_direct_decisions_and_sanitizes_samples() {
        let now = Instant::now();
        let snapshot = snapshot(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, -1.0),
        );
        assert_eq!(
            render_direct(
                &snapshot,
                42,
                &mut [1.0, -1.0],
                1,
                now + ACRE_AUDIO_STATE_TTL + Duration::from_millis(1),
            ),
            AcreDirectRenderAction::Mute
        );

        let mut samples = [f32::NAN, 2.0, -2.0, 0.5];
        assert_eq!(
            render_direct(&snapshot, 42, &mut samples, 2, now),
            AcreDirectRenderAction::Modified
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().all(|sample| sample.abs() <= 1.0));

        assert_eq!(
            render_direct(&snapshot, 42, &mut [0.5, 0.5, 0.5], 2, now),
            AcreDirectRenderAction::Mute
        );
    }

    #[test]
    fn ramps_new_direct_audio_from_silence_and_resets_after_a_stale_decision() {
        let now = Instant::now();
        let snapshot = snapshot(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, 0.0),
        );
        let mut gain_state = AcreDirectGainState::default();
        let mut first = [1.0_f32; 4];
        assert_eq!(
            render_direct_with_gain_state(
                &snapshot,
                42,
                &mut first,
                1,
                48_000,
                now,
                &mut gain_state,
            ),
            AcreDirectRenderAction::Modified
        );
        assert!(
            first
                .iter()
                .zip([0.0, 0.25, 0.5, 0.75])
                .all(|(actual, expected)| (actual - expected).abs() < f32::EPSILON)
        );

        let mut stale = [1.0_f32; 4];
        assert_eq!(
            render_direct_with_gain_state(
                &snapshot,
                42,
                &mut stale,
                1,
                48_000,
                now + ACRE_AUDIO_STATE_TTL + Duration::from_millis(1),
                &mut gain_state,
            ),
            AcreDirectRenderAction::Mute
        );

        let mut recovered = [1.0_f32; 4];
        assert_eq!(
            render_direct_with_gain_state(
                &snapshot,
                42,
                &mut recovered,
                1,
                48_000,
                now,
                &mut gain_state,
            ),
            AcreDirectRenderAction::Modified
        );
        assert!(
            recovered
                .iter()
                .zip([0.0, 0.25, 0.5, 0.75])
                .all(|(actual, expected)| (actual - expected).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn babel_changes_direct_pcm_before_acre_gain_and_pan() {
        let now = Instant::now();
        let dry_snapshot = snapshot(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, 0.0),
        );
        let babel_snapshot = snapshot_with_babel(
            AcreVoiceCurveModel::Original,
            1.0,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, 0.0),
            true,
        );
        assert!(
            direct_plan(&babel_snapshot, 42, now)
                .expect("Babel does not change direct reachability")
                .speaks_babel
        );

        let input = [0.35_f32; 480];
        let mut dry = input;
        let mut babel = input;
        let mut dry_state = AcreDirectGainState::default();
        let mut babel_state = AcreDirectGainState::default();

        assert_eq!(
            render_direct_with_gain_state(
                &dry_snapshot,
                42,
                &mut dry,
                1,
                48_000,
                now,
                &mut dry_state,
            ),
            AcreDirectRenderAction::Modified
        );
        assert_eq!(
            render_direct_with_gain_state(
                &babel_snapshot,
                42,
                &mut babel,
                1,
                48_000,
                now,
                &mut babel_state,
            ),
            AcreDirectRenderAction::Modified
        );

        assert!(babel.iter().all(|sample| sample.is_finite()));
        assert!(
            babel
                .iter()
                .zip(dry)
                .any(|(processed, unmodified)| processed.to_bits() != unmodified.to_bits()),
            "Babel direct audio must not retain the dry pre-gain source"
        );
    }
}
