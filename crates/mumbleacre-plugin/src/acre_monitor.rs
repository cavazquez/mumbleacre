//! Allocation-free renderer for ACRE's non-spatial monitor decisions.
//!
//! ACRE2 emits `g` (God) and `s` (spectator) with an already-authorized
//! volume and no position.  Unlike direct/Zeus speech they therefore do not
//! require a listener pose or a distance curve.  This module applies only
//! that decision's bounded gain and optional Babel flag; it never invents
//! targets, privileges, or a fallback to legacy Mumble routing.

use std::time::Instant;

use mumbleacre_acre::{AcreAudioSnapshot, SpeakingDecision};

use crate::acre_babel::AcreBabelState;
use crate::acre_direct::ACRE_AUDIO_STATE_TTL;

const MIN_AUDIBLE_VOLUME: f32 = 0.001;

/// Precomputed gain for one ACRE God or spectator decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AcreMonitorPlan {
    pub(crate) speaks_babel: bool,
    pub(crate) gain: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcreMonitorRenderAction {
    Modified,
    Mute,
}

/// Persistent per-speaker state for the monitor gain envelope. It is created
/// only by the control plane, so the audio callback never allocates while
/// accepting a `g` or `s` decision.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcreMonitorGainState {
    gain: f32,
    initialized: bool,
    babel: [AcreBabelState; 2],
}

impl Default for AcreMonitorGainState {
    fn default() -> Self {
        Self {
            gain: 0.0,
            initialized: false,
            babel: [AcreBabelState::new(), AcreBabelState::new()],
        }
    }
}

impl AcreMonitorGainState {
    pub(crate) fn reset(&mut self) {
        self.gain = 0.0;
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
        plan: AcreMonitorPlan,
    ) -> bool {
        if !self.apply_babel(samples, channel_count, sample_rate, plan.speaks_babel) {
            return false;
        }
        let start_gain = if self.initialized { self.gain } else { 0.0 };
        match channel_count {
            1 => apply_mono_ramp(samples, start_gain, plan.gain),
            2 => apply_stereo_ramp(samples, start_gain, plan.gain),
            _ => unreachable!("channel count validated before applying ACRE monitor gain"),
        }
        self.gain = plan.gain;
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

/// Builds a plan only for a fresh ACRE God or spectator decision. No listener
/// state is needed: ACRE deliberately made these modes non-spatial.
pub(crate) fn monitor_plan(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    now: Instant,
) -> Option<AcreMonitorPlan> {
    let speaker = snapshot.speaker(speaker_id)?;
    if !speaker.is_fresh(ACRE_AUDIO_STATE_TTL, now) {
        return None;
    }
    let volume = match speaker.decision() {
        SpeakingDecision::Spectator { volume } | SpeakingDecision::God { volume } => *volume,
        _ => return None,
    };
    if !volume.is_finite() || volume <= MIN_AUDIBLE_VOLUME {
        return None;
    }
    Some(AcreMonitorPlan {
        speaks_babel: speaker.speaks_babel(),
        gain: volume.clamp(0.0, 1.0),
    })
}

/// Applies a fresh ACRE monitor decision without allocation, locks, I/O, or
/// Mumble APIs. Missing, stale, malformed, or different decisions reset the
/// previous state and return `Mute` for the caller's fail-closed path.
pub(crate) fn render_monitor_with_gain_state(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    samples: &mut [f32],
    channel_count: usize,
    sample_rate: u32,
    now: Instant,
    gain_state: &mut AcreMonitorGainState,
) -> AcreMonitorRenderAction {
    if samples.is_empty()
        || !(1..=2).contains(&channel_count)
        || !samples.len().is_multiple_of(channel_count)
        || samples.len() > usize::from(u16::MAX)
    {
        gain_state.reset();
        return AcreMonitorRenderAction::Mute;
    }
    let Some(plan) = monitor_plan(snapshot, speaker_id, now) else {
        gain_state.reset();
        return AcreMonitorRenderAction::Mute;
    };
    if gain_state.apply(samples, channel_count, sample_rate, plan) {
        AcreMonitorRenderAction::Modified
    } else {
        gain_state.reset();
        AcreMonitorRenderAction::Mute
    }
}

fn apply_mono_ramp(samples: &mut [f32], previous_gain: f32, target_gain: f32) {
    let sample_count = u16::try_from(samples.len())
        .expect("ACRE monitor renderer rejects buffers longer than u16::MAX samples");
    let step = 1.0 / f32::from(sample_count);
    let mut progress = 0.0;
    for sample in samples {
        let gain = previous_gain + ((target_gain - previous_gain) * progress);
        *sample = scaled_sample(*sample, gain);
        progress += step;
    }
}

fn apply_stereo_ramp(samples: &mut [f32], previous_gain: f32, target_gain: f32) {
    let frame_count = u16::try_from(samples.len() / 2)
        .expect("ACRE monitor renderer rejects buffers longer than u16::MAX frames");
    let step = 1.0 / f32::from(frame_count);
    let mut progress = 0.0;
    for frame in samples.chunks_exact_mut(2) {
        let gain = previous_gain + ((target_gain - previous_gain) * progress);
        frame[0] = scaled_sample(frame[0], gain);
        frame[1] = scaled_sample(frame[1], gain);
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

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{AcreAudioSnapshotBuilder, AcreAudioUpdate, SpeakingUpdate};

    fn snapshot(decision: SpeakingDecision) -> AcreAudioSnapshot {
        let now = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::default();
        builder
            .apply_update(
                AcreAudioUpdate::Speaker(
                    SpeakingUpdate {
                        speaker_id: 42,
                        speaks_babel: false,
                        decision,
                    }
                    .into(),
                ),
                now,
            )
            .unwrap()
    }

    #[test]
    fn god_and_spectator_use_their_acre_volume_without_a_listener() {
        let now = Instant::now();
        let spectator = snapshot(SpeakingDecision::Spectator { volume: 0.6 });
        let god = snapshot(SpeakingDecision::God { volume: 0.9 });

        assert!((monitor_plan(&spectator, 42, now).unwrap().gain - 0.6).abs() < f32::EPSILON);
        assert!((monitor_plan(&god, 42, now).unwrap().gain - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn fades_and_fails_closed_for_stale_or_other_decisions() {
        let now = Instant::now();
        let god_snapshot = snapshot(SpeakingDecision::God { volume: 0.5 });
        let mut state = AcreMonitorGainState::default();
        let mut samples = [f32::NAN, 2.0, -2.0, 0.5];

        assert_eq!(
            render_monitor_with_gain_state(
                &god_snapshot,
                42,
                &mut samples,
                2,
                48_000,
                now,
                &mut state,
            ),
            AcreMonitorRenderAction::Modified
        );
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().all(|sample| sample.abs() <= 1.0));

        let mut stale_samples = [0.5_f32; 4];
        assert_eq!(
            render_monitor_with_gain_state(
                &god_snapshot,
                42,
                &mut stale_samples,
                2,
                48_000,
                now + ACRE_AUDIO_STATE_TTL + std::time::Duration::from_millis(1),
                &mut state,
            ),
            AcreMonitorRenderAction::Mute
        );

        let other = snapshot(SpeakingDecision::Mute);
        assert_eq!(
            render_monitor_with_gain_state(
                &other,
                42,
                &mut stale_samples,
                2,
                48_000,
                now,
                &mut state,
            ),
            AcreMonitorRenderAction::Mute
        );
    }
}
