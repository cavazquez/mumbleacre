//! Allocation-free approximation of ACRE2's `acre_babbel` mono effect.
//!
//! The pinned ACRE2 core inserts this effect before radio filtering and volume
//! whenever `updateSpeakingData` marks a speaker as speaking Babel. Its native
//! implementation modulates the source, passes it through two fourth-order
//! low-pass stages, and applies a bounded gain boost. The state here mirrors
//! that order without allocating from the Mumble callback.

use crate::acre_radio_render::AcreBiquad;

const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 192_000;
const BABEL_BASE_FREQUENCY_HZ: f32 = 5_200.0;
const BABEL_CARRIER_MULTIPLIER: f32 = 0.7;
const BABEL_FIRST_CUTOFF_HZ: f32 = BABEL_BASE_FREQUENCY_HZ * 0.3;
const BABEL_SECOND_CUTOFF_HZ: f32 = BABEL_BASE_FREQUENCY_HZ * 0.5;
const BABEL_VOLUME_MODIFIER: f32 = 6.0;
const BUTTERWORTH_Q_LOW: f32 = 0.541_196_1;
const BUTTERWORTH_Q_HIGH: f32 = 1.306_563;

/// Persistent, per-mono-channel Babel DSP state. Call [`Self::prepare_frame`]
/// once before each PCM buffer and [`Self::process_sample`] for each source
/// sample. The carrier intentionally restarts at the beginning of a buffer to
/// match the native ACRE effect's local sample index.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AcreBabelState {
    sample_rate: u32,
    enabled: bool,
    carrier_phase: f32,
    first_low_pass: [AcreBiquad; 2],
    second_low_pass: [AcreBiquad; 2],
}

impl Default for AcreBabelState {
    fn default() -> Self {
        Self::new()
    }
}

impl AcreBabelState {
    #[must_use]
    pub(crate) fn new() -> Self {
        let mut state = Self {
            sample_rate: 0,
            enabled: false,
            carrier_phase: 0.0,
            first_low_pass: [AcreBiquad::identity(); 2],
            second_low_pass: [AcreBiquad::identity(); 2],
        };
        let _ = state.configure_sample_rate(DEFAULT_SAMPLE_RATE);
        state
    }

    /// Selects whether the current source buffer is Babel. Disabling it clears
    /// the filter history so a language transition cannot leak a tail into a
    /// newly intelligible source. `false` is returned only for an unsupported
    /// sample rate while Babel is requested.
    pub(crate) fn prepare_frame(&mut self, sample_rate: u32, enabled: bool) -> bool {
        if !enabled {
            if self.enabled {
                self.reset();
            }
            self.enabled = false;
            return true;
        }
        if !self.configure_sample_rate(sample_rate) {
            self.reset();
            return false;
        }
        if !self.enabled {
            self.reset_filters();
            self.enabled = true;
        }
        // `CBabbelEffect::process` starts its local `i` at zero every mixer
        // call. Preserve that buffer boundary rather than carrying a carrier
        // phase across arbitrary Mumble packet sizes.
        self.carrier_phase = 0.0;
        true
    }

    /// Applies the enabled effect to one finite source sample. When Babel is
    /// disabled this is a sanitizing identity transform, which lets callers
    /// keep a single source-processing loop with no dry bypass.
    pub(crate) fn process_sample(&mut self, source: f32) -> f32 {
        let source = finite_or_zero(source);
        if !self.enabled {
            return source;
        }

        let modulated = source * self.carrier_phase.cos();
        self.carrier_phase +=
            std::f32::consts::TAU * BABEL_BASE_FREQUENCY_HZ * BABEL_CARRIER_MULTIPLIER
                / sample_rate_as_f32(self.sample_rate);
        if self.carrier_phase >= std::f32::consts::TAU {
            self.carrier_phase -= std::f32::consts::TAU;
        }

        let first = self
            .first_low_pass
            .iter_mut()
            .fold(modulated, |sample, filter| filter.process(sample));
        let second = self
            .second_low_pass
            .iter_mut()
            .fold(first, |sample, filter| filter.process(sample));
        finite_or_zero(second * BABEL_VOLUME_MODIFIER).clamp(-1.0, 1.0)
    }

    pub(crate) fn reset(&mut self) {
        self.enabled = false;
        self.carrier_phase = 0.0;
        self.reset_filters();
    }

    fn configure_sample_rate(&mut self, sample_rate: u32) -> bool {
        if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
            return false;
        }
        if self.sample_rate == sample_rate {
            return true;
        }
        self.sample_rate = sample_rate;
        self.first_low_pass = butterworth_low_pass(sample_rate, BABEL_FIRST_CUTOFF_HZ);
        self.second_low_pass = butterworth_low_pass(sample_rate, BABEL_SECOND_CUTOFF_HZ);
        self.reset_filters();
        true
    }

    fn reset_filters(&mut self) {
        for filter in self
            .first_low_pass
            .iter_mut()
            .chain(self.second_low_pass.iter_mut())
        {
            filter.reset();
        }
    }
}

fn butterworth_low_pass(sample_rate: u32, cutoff_hz: f32) -> [AcreBiquad; 2] {
    [
        AcreBiquad::low_pass(sample_rate, cutoff_hz, BUTTERWORTH_Q_LOW),
        AcreBiquad::low_pass(sample_rate, cutoff_hz, BUTTERWORTH_Q_HIGH),
    ]
}

#[allow(clippy::cast_precision_loss)]
fn sample_rate_as_f32(sample_rate: u32) -> f32 {
    // The supported rates are well below f32's exact integer range.
    sample_rate as f32
}

fn finite_or_zero(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn babel_changes_voice_but_disabled_is_a_sanitizing_identity() {
        let input = [0.35_f32; 480];
        let mut disabled = AcreBabelState::new();
        assert!(disabled.prepare_frame(48_000, false));
        let disabled_output = input.map(|sample| disabled.process_sample(sample));
        assert_eq!(disabled_output.map(f32::to_bits), input.map(f32::to_bits));

        let mut enabled = AcreBabelState::new();
        assert!(enabled.prepare_frame(48_000, true));
        let output = input.map(|sample| enabled.process_sample(sample));
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
        assert!(
            output
                .iter()
                .zip(input)
                .any(|(processed, dry)| processed.to_bits() != dry.to_bits()),
            "enabled Babel must not retain an intelligible dry PCM bypass"
        );
    }

    #[test]
    fn language_toggle_clears_dsp_history_and_invalid_rate_fails_closed() {
        let mut state = AcreBabelState::new();
        assert!(state.prepare_frame(48_000, true));
        for _ in 0..32 {
            let _ = state.process_sample(0.5);
        }
        assert!(state.prepare_frame(48_000, false));
        assert_eq!(state.process_sample(0.25).to_bits(), 0.25_f32.to_bits());
        assert!(!state.prepare_frame(1, true));
    }

    #[test]
    fn steady_babel_processing_does_not_allocate() {
        let mut state = AcreBabelState::new();
        assert!(state.prepare_frame(48_000, true));
        for _ in 0..64 {
            let _ = state.process_sample(0.3);
        }

        let ((), allocations) = crate::test_alloc::count_allocations(|| {
            for _ in 0..20 {
                assert!(state.prepare_frame(48_000, true));
                for _ in 0..480 {
                    let _ = state.process_sample(0.3);
                }
            }
        });
        assert_eq!(allocations, 0, "Babel DSP allocated on the heap");
    }
}
