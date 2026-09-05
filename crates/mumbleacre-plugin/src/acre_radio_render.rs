//! Stateful, allocation-free ACRE radio rendering.
//!
//! ACRE emits one independent mono channel for every radio reception path.
//! This module mirrors that boundary: every path owns its filter, noise,
//! foldback, and volume-envelope state; the callback then mixes only those
//! processed paths into the Mumble buffer.  It deliberately consumes the
//! already-decided paths from [`crate::acre_radio`] and never recreates radio
//! reachability, frequency matching, or propagation on the client.

use std::time::Instant;

use mumbleacre_acre::{ACRE_MAX_RADIO_RECEPTION_PATHS, AcreAudioSnapshot};

use crate::acre_babel::AcreBabelState;
use crate::acre_radio::{AcreRadioPathPlan, plan_radio_paths};

const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 192_000;
const RADIO_BOOST: f32 = 3.0;
const RING_MODULATION_HZ: f32 = 90.0;
const PINK_NOISE_COEFFICIENT: f32 = 0.35;
const WHITE_NOISE_COEFFICIENT: f32 = 0.001;
const WHITE_NOISE_RANGE: f32 = 32_767.0;
const WHITE_NOISE_BUCKET: f32 = 10_923.0;
const LOW_PASS_HZ: f32 = 4_000.0;
const HIGH_PASS_HZ: f32 = 750.0;
const LOUDSPEAKER_SHELF_HZ: f32 = 1_000.0;

/// Result of handling one radio decision in the audio callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcreRadioRenderAction {
    Modified,
    Mute,
}

/// All persistent DSP state for a speaker's bounded radio paths.  It contains
/// no heap buffers, so the control plane can create it before an audio callback
/// ever observes a radio decision.
pub(crate) struct AcreRadioRenderState {
    paths: [AcreRadioPathState; ACRE_MAX_RADIO_RECEPTION_PATHS],
}

impl AcreRadioRenderState {
    #[must_use]
    pub(crate) fn new(speaker_id: u32) -> Self {
        Self {
            paths: std::array::from_fn(|path_index| {
                AcreRadioPathState::new(path_seed(speaker_id, path_index))
            }),
        }
    }

    pub(crate) fn reset(&mut self) {
        for path in &mut self.paths {
            path.reset();
        }
    }

    fn render(
        &mut self,
        samples: &mut [f32],
        channel_count: usize,
        sample_rate: u32,
        plans: &[AcreRadioPathPlan],
        speaks_babel: bool,
    ) -> AcreRadioRenderAction {
        if plans.is_empty()
            || !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate)
            || !plans.iter().any(|plan| plan.signal_quality > 0.0)
        {
            self.reset();
            return AcreRadioRenderAction::Mute;
        }

        for plan in plans {
            self.paths[plan.path_index].configure_sample_rate(sample_rate);
            if !self.paths[plan.path_index].prepare_babel_frame(sample_rate, speaks_babel) {
                self.reset();
                return AcreRadioRenderAction::Mute;
            }
        }

        let frame_count = u16::try_from(samples.len() / channel_count)
            .expect("radio renderer validates the maximum PCM buffer length");
        let step = 1.0 / f32::from(frame_count);
        let mut progress = 0.0;

        for frame in samples.chunks_exact_mut(channel_count) {
            // ACRE's mono channel captures the first source channel when the
            // VOIP host delivers an interleaved stereo frame.  Each radio path
            // receives that same source, then owns an independent DSP chain.
            let source = finite_or_zero(frame[0]);
            let mut left = 0.0;
            let mut right = 0.0;

            for plan in plans {
                let state = &mut self.paths[plan.path_index];
                let filtered = state.filter_sample(
                    source,
                    plan.signal_quality,
                    plan.signal_model,
                    plan.loudspeaker,
                    true,
                );
                let volume = state.volume_at(progress, plan.volume);
                match channel_count {
                    1 => {
                        left += filtered * volume * plan.spatial_mono_gain;
                    }
                    2 => {
                        left += filtered * volume * plan.spatial_left_gain;
                        right += filtered * volume * plan.spatial_right_gain;
                    }
                    _ => unreachable!("channel count is validated before radio rendering"),
                }
            }

            frame[0] = normalized_mix(left);
            if channel_count == 2 {
                frame[1] = normalized_mix(right);
            }
            progress += step;
        }

        for plan in plans {
            self.paths[plan.path_index].commit_volume(plan.volume);
        }
        AcreRadioRenderAction::Modified
    }
}

/// Builds radio plans from the current immutable ACRE snapshot and renders
/// them in place.  Invalid, stale, empty, or zero-quality decisions fail
/// closed; no source PCM survives as a dry path.
pub(crate) fn render_radio_with_state(
    snapshot: &AcreAudioSnapshot,
    speaker_id: u32,
    samples: &mut [f32],
    channel_count: usize,
    sample_rate: u32,
    now: Instant,
    state: &mut AcreRadioRenderState,
) -> AcreRadioRenderAction {
    if samples.is_empty()
        || !(1..=2).contains(&channel_count)
        || !samples.len().is_multiple_of(channel_count)
        || samples.len() > usize::from(u16::MAX)
    {
        state.reset();
        return AcreRadioRenderAction::Mute;
    }

    let mut plans = [AcreRadioPathPlan::SILENT; ACRE_MAX_RADIO_RECEPTION_PATHS];
    let Some(path_count) = plan_radio_paths(snapshot, speaker_id, now, &mut plans) else {
        state.reset();
        return AcreRadioRenderAction::Mute;
    };
    let speaks_babel = snapshot
        .speaker(speaker_id)
        .is_some_and(mumbleacre_acre::AcreSpeakerSnapshot::speaks_babel);
    state.render(
        samples,
        channel_count,
        sample_rate,
        &plans[..path_count],
        speaks_babel,
    )
}

#[derive(Clone, Copy)]
pub(crate) struct AcreRadioPathState {
    seed: u32,
    random_state: u32,
    pink_state: [f32; 3],
    ring_phase: f32,
    foldback_value: f32,
    foldback_remaining: u8,
    previous_volume: f32,
    volume_initialized: bool,
    sample_rate: u32,
    low_pass: AcreBiquad,
    high_pass: AcreBiquad,
    loudspeaker_shelf: AcreBiquad,
    babel: AcreBabelState,
}

impl AcreRadioPathState {
    pub(crate) fn new(seed: u32) -> Self {
        let mut state = Self {
            seed,
            random_state: seed,
            pink_state: [0.0; 3],
            ring_phase: 0.0,
            foldback_value: 0.0,
            foldback_remaining: 0,
            previous_volume: 0.0,
            volume_initialized: false,
            sample_rate: 0,
            low_pass: AcreBiquad::identity(),
            high_pass: AcreBiquad::identity(),
            loudspeaker_shelf: AcreBiquad::identity(),
            babel: AcreBabelState::new(),
        };
        state.configure_sample_rate(DEFAULT_SAMPLE_RATE);
        state
    }

    pub(crate) fn reset(&mut self) {
        self.random_state = self.seed;
        self.pink_state = [0.0; 3];
        self.ring_phase = 0.0;
        self.foldback_value = 0.0;
        self.foldback_remaining = 0;
        self.previous_volume = 0.0;
        self.volume_initialized = false;
        self.low_pass.reset();
        self.high_pass.reset();
        self.loudspeaker_shelf.reset();
        self.babel.reset();
    }

    pub(crate) fn configure_sample_rate(&mut self, sample_rate: u32) {
        if self.sample_rate == sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.low_pass = AcreBiquad::low_pass(sample_rate, LOW_PASS_HZ, 2.0);
        self.high_pass = AcreBiquad::high_pass(sample_rate, HIGH_PASS_HZ, 0.97);
        self.loudspeaker_shelf =
            AcreBiquad::low_shelf(sample_rate, LOUDSPEAKER_SHELF_HZ, -10.0, 1.0);
        self.reset();
    }

    pub(crate) fn filter_sample(
        &mut self,
        source: f32,
        signal_quality: f32,
        signal_model: f32,
        loudspeaker: bool,
        noise_enabled: bool,
    ) -> f32 {
        if signal_quality <= 0.0 {
            return 0.0;
        }

        let source = self.babel.process_sample(source);

        // `signalModel` is carried by updateSpeakingData, but the pinned
        // ACRE2 v2.14 CFilterRadio stores no behavior behind it: its filter
        // reads signalQuality and loudspeaker only.  Preserve it at the
        // planner boundary and do not invent a second propagation model here.
        let _ = signal_model;

        let inverse_quality = 1.25 - signal_quality;
        let mut sample = source * RADIO_BOOST;
        if noise_enabled {
            let pink_noise = self.pink_noise() * (PINK_NOISE_COEFFICIENT * inverse_quality);
            sample = (sample + pink_noise) - (pink_noise * sample);
            sample += self.white_noise() * (WHITE_NOISE_COEFFICIENT * inverse_quality);
        }
        sample = self.ring_modulate(sample, signal_quality);
        sample = self.foldback(sample, signal_quality);
        sample = self.low_pass.process(sample);
        sample = self.high_pass.process(sample);
        if loudspeaker {
            sample = self.loudspeaker_shelf.process(sample);
        }
        finite_or_zero(sample).clamp(-1.0, 1.0)
    }

    pub(crate) fn prepare_babel_frame(&mut self, sample_rate: u32, speaks_babel: bool) -> bool {
        self.babel.prepare_frame(sample_rate, speaks_babel)
    }

    fn pink_noise(&mut self) -> f32 {
        const COEFFICIENTS: [f32; 3] = [0.021_092_38, 0.071_134_78, 0.688_735_6];
        const POLES: [f32; 3] = [0.319, 0.775_6, 0.961_3];

        for (index, pole) in POLES.iter().enumerate() {
            let random = self.next_unit();
            self.pink_state[index] = *pole * (self.pink_state[index] - random) + random;
        }
        let weighted = COEFFICIENTS[0] * self.pink_state[0]
            + COEFFICIENTS[1] * self.pink_state[1]
            + COEFFICIENTS[2] * self.pink_state[2];
        (2.0 * weighted) - (COEFFICIENTS[0] + COEFFICIENTS[1] + COEFFICIENTS[2])
    }

    fn white_noise(&mut self) -> f32 {
        // This follows ACRE's intentionally small, quantized white-noise
        // helper while using a path-local deterministic generator instead of
        // its process-global C rand() state.
        let random = self.next_unit();
        ((6.0 * random * WHITE_NOISE_BUCKET) - (3.0 * (WHITE_NOISE_BUCKET - 1.0)))
            / WHITE_NOISE_RANGE
    }

    fn next_unit(&mut self) -> f32 {
        self.random_state = self
            .random_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let upper = u16::try_from(self.random_state >> 16).unwrap_or_default();
        f32::from(upper) / f32::from(u16::MAX)
    }

    fn ring_modulate(&mut self, sample: f32, signal_quality: f32) -> f32 {
        let mix = (1.0 - signal_quality) * 0.20;
        let modulated = sample * (self.ring_phase * std::f32::consts::FRAC_PI_2).sin();
        self.ring_phase += RING_MODULATION_HZ / sample_rate_as_f32(self.sample_rate);
        if self.ring_phase > 1.0 {
            self.ring_phase = 0.0;
        }
        (sample * (1.0 - mix)) + (modulated * mix)
    }

    fn foldback(&mut self, sample: f32, signal_quality: f32) -> f32 {
        if self.foldback_remaining == 0 {
            self.foldback_value = sample;
            self.foldback_remaining = foldback_divisor(signal_quality).saturating_sub(1);
            sample
        } else {
            self.foldback_remaining -= 1;
            self.foldback_value
        }
    }

    pub(crate) fn volume_at(&self, progress: f32, target_volume: f32) -> f32 {
        let initial = if self.volume_initialized {
            self.previous_volume
        } else {
            0.0
        };
        initial + ((target_volume - initial) * progress)
    }

    pub(crate) fn commit_volume(&mut self, target_volume: f32) {
        self.previous_volume = target_volume;
        self.volume_initialized = true;
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AcreBiquad {
    a1: f32,
    a2: f32,
    b0: f32,
    b1: f32,
    b2: f32,
    v1: f32,
    v2: f32,
}

impl AcreBiquad {
    pub(crate) const fn identity() -> Self {
        Self {
            a1: 0.0,
            a2: 0.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            v1: 0.0,
            v2: 0.0,
        }
    }

    pub(crate) fn low_pass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Self {
        let (sine, cosine) = normalized_frequency(sample_rate, cutoff_hz).sin_cos();
        let alpha = sine / (2.0 * q);
        Self::from_coefficients(
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
            (1.0 - cosine) * 0.5,
            1.0 - cosine,
            (1.0 - cosine) * 0.5,
        )
    }

    fn high_pass(sample_rate: u32, cutoff_hz: f32, q: f32) -> Self {
        let (sine, cosine) = normalized_frequency(sample_rate, cutoff_hz).sin_cos();
        let alpha = sine / (2.0 * q);
        Self::from_coefficients(
            1.0 + alpha,
            -2.0 * cosine,
            1.0 - alpha,
            (1.0 + cosine) * 0.5,
            -(1.0 + cosine),
            (1.0 + cosine) * 0.5,
        )
    }

    fn low_shelf(sample_rate: u32, cutoff_hz: f32, gain_db: f32, slope: f32) -> Self {
        let amplitude = 10.0_f32.powf(gain_db / 40.0);
        let (sine, cosine) = normalized_frequency(sample_rate, cutoff_hz).sin_cos();
        let alpha =
            (sine * 0.5) * (((amplitude + (1.0 / amplitude)) * ((1.0 / slope) - 1.0)) + 2.0).sqrt();
        let square = 2.0 * amplitude.sqrt() * alpha;
        Self::from_coefficients(
            (amplitude + 1.0) + ((amplitude - 1.0) * cosine) + square,
            -2.0 * ((amplitude - 1.0) + ((amplitude + 1.0) * cosine)),
            (amplitude + 1.0) + ((amplitude - 1.0) * cosine) - square,
            amplitude * ((amplitude + 1.0) - ((amplitude - 1.0) * cosine) + square),
            2.0 * amplitude * ((amplitude - 1.0) - ((amplitude + 1.0) * cosine)),
            amplitude * ((amplitude + 1.0) - ((amplitude - 1.0) * cosine) - square),
        )
    }

    fn from_coefficients(a0: f32, a1: f32, a2: f32, b0: f32, b1: f32, b2: f32) -> Self {
        Self {
            a1: a1 / a0,
            a2: a2 / a0,
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            v1: 0.0,
            v2: 0.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.v1 = 0.0;
        self.v2 = 0.0;
    }

    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let state = input - (self.a1 * self.v1) - (self.a2 * self.v2);
        let output = (self.b0 * state) + (self.b1 * self.v1) + (self.b2 * self.v2);
        if !state.is_finite() || !output.is_finite() {
            self.reset();
            return 0.0;
        }
        self.v2 = self.v1;
        self.v1 = state;
        output
    }
}

fn normalized_frequency(sample_rate: u32, cutoff_hz: f32) -> f32 {
    std::f32::consts::TAU * cutoff_hz / sample_rate_as_f32(sample_rate)
}

#[allow(clippy::cast_precision_loss)]
fn sample_rate_as_f32(sample_rate: u32) -> f32 {
    // The renderer rejects rates above 192 kHz, well below the exact-integer
    // range of f32, so this conversion is precise.
    sample_rate as f32
}

fn foldback_divisor(signal_quality: f32) -> u8 {
    let candidate = 256.0 * signal_quality.powi(4) - (693.33 * signal_quality.powi(3))
        + (648.0 * signal_quality.powi(2))
        - (250.67 * signal_quality)
        + 40.0;
    let bounded = candidate.floor().clamp(5.0, f32::from(u8::MAX));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        bounded as u8
    }
}

fn normalized_mix(sample: f32) -> f32 {
    finite_or_zero(sample).clamp(-1.0, 1.0)
}

fn finite_or_zero(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

pub(crate) fn path_seed(speaker_id: u32, path_index: usize) -> u32 {
    let path = u32::try_from(path_index).unwrap_or_default();
    let seed = speaker_id
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(path.wrapping_mul(0x85eb_ca6b))
        ^ 0xa341_316c;
    if seed == 0 { 1 } else { seed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{
        AcreAudioSnapshotBuilder, AcreAudioUpdate, AcreListenerPose, AcreListenerState,
        AcreSpeakerAudioUpdate, AcreSpeakerVector, AcreVoiceCurveModel, RadioReceptionPath,
        SpeakingDecision, SpeakingUpdate,
    };

    fn vector(x: f32, y: f32, z: f32) -> AcreSpeakerVector {
        AcreSpeakerVector { x, z, y }
    }

    fn snapshot(paths: Vec<RadioReceptionPath>, now: Instant) -> AcreAudioSnapshot {
        snapshot_with_babel(paths, false, now)
    }

    fn snapshot_with_babel(
        paths: Vec<RadioReceptionPath>,
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
                            decision: SpeakingDecision::Radio { paths },
                        },
                        1.0,
                    )
                    .unwrap(),
                ),
                now,
            )
            .unwrap()
    }

    fn path(position: AcreSpeakerVector, quality: f32) -> RadioReceptionPath {
        RadioReceptionPath {
            volume: 0.65,
            signal_quality: quality,
            signal_model: 3.0,
            loudspeaker: false,
            position,
        }
    }

    fn input() -> Vec<f32> {
        let mut samples = vec![0.0; 480 * 2];
        for frame in samples.chunks_exact_mut(2) {
            frame[0] = 0.45;
            frame[1] = -0.45;
        }
        samples
    }

    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|sample| sample.abs()).sum()
    }

    #[test]
    fn mixes_all_paths_without_leaving_a_dry_source() {
        let now = Instant::now();
        let one_path = snapshot(vec![path(vector(0.0, 0.0, 0.0), 0.8)], now);
        let two_paths = snapshot(
            vec![
                path(vector(0.0, 0.0, 0.0), 0.8),
                path(vector(0.0, 0.0, 0.0), 0.8),
            ],
            now,
        );
        let original = input();
        let mut one = original.clone();
        let mut two = original.clone();
        let mut one_state = AcreRadioRenderState::new(42);
        let mut two_state = AcreRadioRenderState::new(42);

        assert_eq!(
            render_radio_with_state(
                &one_path,
                42,
                &mut one,
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut one_state,
            ),
            AcreRadioRenderAction::Modified
        );
        assert_eq!(
            render_radio_with_state(
                &two_paths,
                42,
                &mut two,
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut two_state,
            ),
            AcreRadioRenderAction::Modified
        );

        assert_ne!(
            one, original,
            "radio rendering must replace the dry Mumble source"
        );
        assert!(one.iter().all(|sample| sample.is_finite()));
        assert!(two.iter().all(|sample| sample.is_finite()));
        assert!(
            energy(&two) > energy(&one) * 1.4,
            "two independently rendered ACRE paths must both reach the mix"
        );
    }

    #[test]
    fn signal_quality_changes_the_radio_effect_without_reinterpreting_model() {
        let now = Instant::now();
        let clear = snapshot(vec![path(vector(-2.0, 0.0, 0.0), 1.0)], now);
        let degraded = snapshot(vec![path(vector(-2.0, 0.0, 0.0), 0.2)], now);
        let mut clear_samples = input();
        let mut degraded_samples = input();
        let mut clear_state = AcreRadioRenderState::new(42);
        let mut degraded_state = AcreRadioRenderState::new(42);

        let _ = render_radio_with_state(
            &clear,
            42,
            &mut clear_samples,
            2,
            DEFAULT_SAMPLE_RATE,
            now,
            &mut clear_state,
        );
        let _ = render_radio_with_state(
            &degraded,
            42,
            &mut degraded_samples,
            2,
            DEFAULT_SAMPLE_RATE,
            now,
            &mut degraded_state,
        );

        assert_ne!(clear_samples, degraded_samples);
        assert!(clear_samples.iter().all(|sample| sample.is_finite()));
        assert!(degraded_samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn zero_quality_or_invalid_audio_shape_fails_closed() {
        let now = Instant::now();
        let no_signal = snapshot(vec![path(vector(0.0, 0.0, 0.0), 0.0)], now);
        let mut state = AcreRadioRenderState::new(42);
        let mut samples = input();

        assert_eq!(
            render_radio_with_state(
                &no_signal,
                42,
                &mut samples,
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut state,
            ),
            AcreRadioRenderAction::Mute
        );
        assert_eq!(
            render_radio_with_state(
                &no_signal,
                42,
                &mut samples[..3],
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut state,
            ),
            AcreRadioRenderAction::Mute
        );
    }

    #[test]
    fn steady_radio_renderer_does_not_allocate() {
        let now = Instant::now();
        let snapshot = snapshot(
            vec![
                path(vector(-2.0, 0.0, 0.0), 0.8),
                path(vector(2.0, 0.0, 0.0), 0.6),
            ],
            now,
        );
        let original = input();
        let mut samples = original.clone();
        let mut state = AcreRadioRenderState::new(42);
        let _ = render_radio_with_state(
            &snapshot,
            42,
            &mut samples,
            2,
            DEFAULT_SAMPLE_RATE,
            now,
            &mut state,
        );

        let ((), allocations) = crate::test_alloc::count_allocations(|| {
            for _ in 0..20 {
                samples.copy_from_slice(&original);
                assert_eq!(
                    render_radio_with_state(
                        &snapshot,
                        42,
                        &mut samples,
                        2,
                        DEFAULT_SAMPLE_RATE,
                        now,
                        &mut state,
                    ),
                    AcreRadioRenderAction::Modified
                );
            }
        });
        assert_eq!(allocations, 0, "radio renderer allocated on the audio path");
    }

    #[test]
    fn babel_changes_each_radio_path_before_radio_dsp() {
        let now = Instant::now();
        let dry_snapshot = snapshot(vec![path(vector(-2.0, 0.0, 0.0), 1.0)], now);
        let babel_snapshot =
            snapshot_with_babel(vec![path(vector(-2.0, 0.0, 0.0), 1.0)], true, now);
        let original = input();
        let mut dry = original.clone();
        let mut babel = original;
        let mut dry_state = AcreRadioRenderState::new(42);
        let mut babel_state = AcreRadioRenderState::new(42);

        assert_eq!(
            render_radio_with_state(
                &dry_snapshot,
                42,
                &mut dry,
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut dry_state,
            ),
            AcreRadioRenderAction::Modified
        );
        assert_eq!(
            render_radio_with_state(
                &babel_snapshot,
                42,
                &mut babel,
                2,
                DEFAULT_SAMPLE_RATE,
                now,
                &mut babel_state,
            ),
            AcreRadioRenderAction::Modified
        );

        assert!(babel.iter().all(|sample| sample.is_finite()));
        assert!(
            babel
                .iter()
                .zip(dry)
                .any(|(processed, unmodified)| processed.to_bits() != unmodified.to_bits()),
            "Babel must alter the source before the ACRE radio filter"
        );
    }
}
