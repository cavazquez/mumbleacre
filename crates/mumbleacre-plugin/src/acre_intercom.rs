//! Allocation-free ACRE intercom renderer.
//!
//! ACRE emits intercom as the spatial `i` decision. Its native client runs
//! that audio through the radio filter at full quality with radio noise
//! disabled, applies the ACRE-provided volume envelope, then mixes it in world
//! space with the stock radio distance curve. This module owns that exact
//! callback boundary without reconstructing vehicle, crew, or intercom-channel
//! eligibility; those policy decisions remain with ACRE's control plane.

use std::time::Instant;

use mumbleacre_acre::{AcreAudioSnapshot, SpatialSpeakingKind, SpeakingDecision};

use crate::acre_direct::ACRE_AUDIO_STATE_TTL;
use crate::acre_radio::radio_spatial_gains;
use crate::acre_radio_render::{AcreRadioPathState, path_seed};

const MIN_AUDIBLE_VOLUME: f32 = 0.001;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 192_000;
const INTERCOM_SIGNAL_QUALITY: f32 = 1.0;
const INTERCOM_SIGNAL_MODEL: f32 = 0.0;

/// Precomputed ACRE intercom mixdown. The spatial gains deliberately remain
/// separate from `volume`, matching the native effect order: radio filter,
/// ACRE volume ramp, then positional mixdown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AcreIntercomPlan {
    pub(crate) distance_m: f32,
    pub(crate) volume: f32,
    pub(crate) spatial_mono_gain: f32,
    pub(crate) spatial_left_gain: f32,
    pub(crate) spatial_right_gain: f32,
    pub(crate) mono_gain: f32,
    pub(crate) left_gain: f32,
    pub(crate) right_gain: f32,
}

/// Result of applying an intercom decision in the Mumble callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcreIntercomRenderAction {
    Modified,
    Mute,
}

/// Persistent filter and volume-ramp state for one speaker. It is created by
/// the control plane before the callback observes an intercom decision.
pub(crate) struct AcreIntercomRenderState {
    path: AcreRadioPathState,
}

impl AcreIntercomRenderState {
    #[must_use]
    pub(crate) fn new(speaker_id: u32) -> Self {
        Self {
            path: AcreRadioPathState::new(path_seed(speaker_id, 0)),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.path.reset();
    }

    fn render(
        &mut self,
        samples: &mut [f32],
        channel_count: usize,
        sample_rate: u32,
        plan: AcreIntercomPlan,
        speaks_babel: bool,
    ) -> AcreIntercomRenderAction {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            self.reset();
            return AcreIntercomRenderAction::Mute;
        }

        self.path.configure_sample_rate(sample_rate);
        if !self.path.prepare_babel_frame(sample_rate, speaks_babel) {
            self.reset();
            return AcreIntercomRenderAction::Mute;
        }
        let frame_count = u16::try_from(samples.len() / channel_count)
            .expect("intercom renderer validates the maximum PCM buffer length");
        let step = 1.0 / f32::from(frame_count);
        let mut progress = 0.0;

        for frame in samples.chunks_exact_mut(channel_count) {
            // The native ACRE channel is mono: if Mumble supplied an
            // interleaved stereo source, retain its first source channel and
            // replace all output channels with the positional mixdown.
            let source = finite_or_zero(frame[0]);
            let filtered = self.path.filter_sample(
                source,
                INTERCOM_SIGNAL_QUALITY,
                INTERCOM_SIGNAL_MODEL,
                false,
                false,
            );
            let volume = self.path.volume_at(progress, plan.volume);
            match channel_count {
                1 => {
                    frame[0] = normalized_mix(filtered * volume * plan.spatial_mono_gain);
                }
                2 => {
                    frame[0] = normalized_mix(filtered * volume * plan.spatial_left_gain);
                    frame[1] = normalized_mix(filtered * volume * plan.spatial_right_gain);
                }
                _ => unreachable!("channel count is validated before intercom rendering"),
            }
            progress += step;
        }

        self.path.commit_volume(plan.volume);
        AcreIntercomRenderAction::Modified
    }
}

/// Plans a single ACRE intercom decision. Missing, stale, non-intercom,
/// malformed, inaudible, or out-of-curve decisions fail closed. In particular,
/// this consumes the already-calculated `volume` from ACRE and never infers
/// vehicle membership, intercom channel, radio state, or a legacy voice curve.
pub(crate) fn intercom_plan(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    now: Instant,
) -> Option<AcreIntercomPlan> {
    let listener = snapshot.listener()?;
    let speaker = snapshot.speaker(speaker_id)?;
    if !listener.is_fresh(ACRE_AUDIO_STATE_TTL, now) || !speaker.is_fresh(ACRE_AUDIO_STATE_TTL, now)
    {
        return None;
    }
    let SpeakingDecision::Spatial {
        kind: SpatialSpeakingKind::Intercom,
        volume,
        position,
        head_vector,
    } = speaker.decision()
    else {
        return None;
    };
    if !volume.is_finite() || *volume <= MIN_AUDIBLE_VOLUME {
        return None;
    }

    let spatial = radio_spatial_gains(listener.pose(), *position, *head_vector)?;
    let mono_gain = (*volume * spatial.mono_gain).clamp(0.0, 1.0);
    let left_gain = (*volume * spatial.left_gain).clamp(0.0, 1.0);
    let right_gain = (*volume * spatial.right_gain).clamp(0.0, 1.0);
    if mono_gain <= MIN_AUDIBLE_VOLUME {
        return None;
    }
    Some(AcreIntercomPlan {
        distance_m: spatial.distance_m,
        volume: *volume,
        spatial_mono_gain: spatial.mono_gain,
        spatial_left_gain: spatial.left_gain,
        spatial_right_gain: spatial.right_gain,
        mono_gain,
        left_gain,
        right_gain,
    })
}

/// Renders one current intercom snapshot in place. No source PCM survives as
/// a dry path. The state must have been prepared off the callback thread.
pub(crate) fn render_intercom_with_state(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    samples: &mut [f32],
    channel_count: usize,
    sample_rate: u32,
    now: Instant,
    state: &mut AcreIntercomRenderState,
) -> AcreIntercomRenderAction {
    if samples.is_empty()
        || !(1..=2).contains(&channel_count)
        || !samples.len().is_multiple_of(channel_count)
        || samples.len() > usize::from(u16::MAX)
    {
        state.reset();
        return AcreIntercomRenderAction::Mute;
    }
    let Some(plan) = intercom_plan(snapshot, speaker_id, now) else {
        state.reset();
        return AcreIntercomRenderAction::Mute;
    };
    let speaks_babel = snapshot
        .speaker(speaker_id)
        .is_some_and(mumbleacre_acre::AcreSpeakerSnapshot::speaks_babel);
    state.render(samples, channel_count, sample_rate, plan, speaks_babel)
}

fn normalized_mix(sample: f32) -> f32 {
    finite_or_zero(sample).clamp(-1.0, 1.0)
}

fn finite_or_zero(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use mumbleacre_acre::{
        AcreAudioSnapshotBuilder, AcreAudioUpdate, AcreListenerPose, AcreListenerState,
        AcreSpeakerAudioUpdate, AcreSpeakerVector, AcreVoiceCurveModel, SpeakingUpdate,
    };

    fn vector(x: f32, y: f32, z: f32) -> AcreSpeakerVector {
        AcreSpeakerVector { x, z, y }
    }

    fn snapshot(
        kind: SpatialSpeakingKind,
        volume: f32,
        position: AcreSpeakerVector,
        head_vector: AcreSpeakerVector,
        now: Instant,
    ) -> AcreAudioSnapshot {
        snapshot_with_babel(kind, volume, position, head_vector, false, now)
    }

    fn snapshot_with_babel(
        kind: SpatialSpeakingKind,
        volume: f32,
        position: AcreSpeakerVector,
        head_vector: AcreSpeakerVector,
        speaks_babel: bool,
        now: Instant,
    ) -> AcreAudioSnapshot {
        let mut builder = AcreAudioSnapshotBuilder::default();
        builder
            .apply_update(
                AcreAudioUpdate::Listener(AcreListenerState {
                    pose: AcreListenerPose {
                        position: vector(0.0, 0.0, 0.0),
                        head_vector: vector(0.0, 0.0, 1.0),
                    },
                    curve_model: AcreVoiceCurveModel::Original,
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
                                volume,
                                position,
                                head_vector,
                            },
                        },
                        1.0,
                    )
                    .unwrap(),
                ),
                now,
            )
            .unwrap()
    }

    #[test]
    fn uses_the_radio_curve_and_actual_intercom_head_vector() {
        let now = Instant::now();
        let position = vector(0.0, 0.0, 10.0);
        let facing_listener = snapshot(
            SpatialSpeakingKind::Intercom,
            0.5,
            position,
            vector(0.0, 0.0, -1.0),
            now,
        );
        let facing_away = snapshot(
            SpatialSpeakingKind::Intercom,
            0.5,
            position,
            vector(0.0, 0.0, 1.0),
            now,
        );

        let toward = intercom_plan(&facing_listener, 42, now).unwrap();
        let away = intercom_plan(&facing_away, 42, now).unwrap();
        let expected_distance_gain = (25.0_f32 / 30.0).powi(2);

        assert!((toward.distance_m - 10.0).abs() < f32::EPSILON);
        assert!((toward.spatial_mono_gain - (expected_distance_gain * 1.2)).abs() < 0.000_1);
        assert!(
            toward.spatial_mono_gain > away.spatial_mono_gain,
            "the remote intercom head vector must affect the source cone"
        );

        let out_of_curve = snapshot(
            SpatialSpeakingKind::Intercom,
            1.0,
            vector(0.0, 0.0, 151.0),
            vector(0.0, 0.0, -1.0),
            now,
        );
        assert_eq!(intercom_plan(&out_of_curve, 42, now), None);
    }

    #[test]
    fn fails_closed_for_stale_or_non_intercom_decisions() {
        let now = Instant::now();
        let intercom = snapshot(
            SpatialSpeakingKind::Intercom,
            0.8,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, -1.0),
            now,
        );
        let direct = snapshot(
            SpatialSpeakingKind::Direct,
            0.8,
            vector(0.0, 0.0, 1.0),
            vector(0.0, 0.0, -1.0),
            now,
        );

        assert_eq!(
            intercom_plan(
                &intercom,
                42,
                now + ACRE_AUDIO_STATE_TTL + Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(intercom_plan(&direct, 42, now), None);
    }

    #[test]
    fn is_noiseless_at_zero_input_and_replaces_dry_pcm() {
        let now = Instant::now();
        let snapshot = snapshot(
            SpatialSpeakingKind::Intercom,
            0.7,
            vector(-2.0, 0.0, 0.0),
            vector(0.0, 0.0, 1.0),
            now,
        );
        let mut state = AcreIntercomRenderState::new(42);
        let mut silent = [0.0_f32; 480 * 2];

        assert_eq!(
            render_intercom_with_state(&snapshot, 42, &mut silent, 2, 48_000, now, &mut state),
            AcreIntercomRenderAction::Modified
        );
        assert!(
            silent.iter().all(|sample| *sample == 0.0),
            "ACRE disables radio noise for intercom"
        );

        let input = [0.3_f32; 480 * 2];
        let mut rendered = input;
        assert_eq!(
            render_intercom_with_state(&snapshot, 42, &mut rendered, 2, 48_000, now, &mut state),
            AcreIntercomRenderAction::Modified
        );
        assert!(rendered.iter().all(|sample| sample.is_finite()));
        assert!(
            rendered
                .iter()
                .zip(input)
                .any(|(processed, dry)| processed.to_bits() != dry.to_bits()),
            "intercom must replace dry Mumble PCM"
        );
    }

    #[test]
    fn steady_intercom_renderer_does_not_allocate() {
        let now = Instant::now();
        let snapshot = snapshot(
            SpatialSpeakingKind::Intercom,
            0.7,
            vector(2.0, 0.0, 0.0),
            vector(0.0, 0.0, -1.0),
            now,
        );
        let mut state = AcreIntercomRenderState::new(42);
        let mut samples = [0.2_f32; 480 * 2];
        let _ = render_intercom_with_state(&snapshot, 42, &mut samples, 2, 48_000, now, &mut state);

        let ((), allocations) = crate::test_alloc::count_allocations(|| {
            for _ in 0..20 {
                samples.fill(0.2);
                assert_eq!(
                    render_intercom_with_state(
                        &snapshot,
                        42,
                        &mut samples,
                        2,
                        48_000,
                        now,
                        &mut state,
                    ),
                    AcreIntercomRenderAction::Modified
                );
            }
        });
        assert_eq!(
            allocations, 0,
            "ACRE intercom renderer allocated on the heap"
        );
    }

    #[test]
    fn babel_changes_intercom_pcm_before_the_noiseless_radio_filter() {
        let now = Instant::now();
        let dry_snapshot = snapshot(
            SpatialSpeakingKind::Intercom,
            0.7,
            vector(-2.0, 0.0, 0.0),
            vector(0.0, 0.0, 1.0),
            now,
        );
        let babel_snapshot = snapshot_with_babel(
            SpatialSpeakingKind::Intercom,
            0.7,
            vector(-2.0, 0.0, 0.0),
            vector(0.0, 0.0, 1.0),
            true,
            now,
        );
        let input = [0.35_f32; 480 * 2];
        let mut dry = input;
        let mut babel = input;
        let mut dry_state = AcreIntercomRenderState::new(42);
        let mut babel_state = AcreIntercomRenderState::new(42);

        assert_eq!(
            render_intercom_with_state(&dry_snapshot, 42, &mut dry, 2, 48_000, now, &mut dry_state,),
            AcreIntercomRenderAction::Modified
        );
        assert_eq!(
            render_intercom_with_state(
                &babel_snapshot,
                42,
                &mut babel,
                2,
                48_000,
                now,
                &mut babel_state,
            ),
            AcreIntercomRenderAction::Modified
        );

        assert!(babel.iter().all(|sample| sample.is_finite()));
        assert!(
            babel
                .iter()
                .zip(dry)
                .any(|(processed, unmodified)| processed.to_bits() != unmodified.to_bits()),
            "Babel must alter intercom source PCM before radio-style filtering"
        );
    }
}
