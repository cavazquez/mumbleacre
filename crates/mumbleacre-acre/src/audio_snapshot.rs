//! Immutable, bounded ACRE audio decisions for a future Mumble RT publisher.
//!
//! ACRE emits one validated [`crate::SpeakingUpdate`] per remote Mumble voice
//! client. This module retains the latest decision for each client together
//! with its pipe generation, monotonically increasing sequence, and timestamp.
//! It deliberately does not touch PCM, Mumble APIs, threads, or atomics; the
//! plugin owns publication to its real-time callback.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{
    AcreListenerState, AcreSpeakerAudioUpdate, AcreVoiceCurveModel, SpeakingDecision,
    SpeakingUpdate,
};

/// Match the existing RT capacity while the ACRE path is being integrated.
/// A full directory rejects a new speaker fail-closed but still accepts
/// refreshes for existing speakers.
pub const ACRE_AUDIO_MAX_SPEAKERS: usize = 128;

/// Latest validated decision for one Mumble voice client.
#[derive(Clone, Debug, PartialEq)]
pub struct AcreSpeakerSnapshot {
    speaker_id: u32,
    generation: u64,
    sequence: u64,
    published_at: Instant,
    speaks_babel: bool,
    curve_scale: f32,
    decision: SpeakingDecision,
}

impl AcreSpeakerSnapshot {
    #[must_use]
    pub const fn speaker_id(&self) -> u32 {
        self.speaker_id
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn published_at(&self) -> Instant {
        self.published_at
    }

    #[must_use]
    pub const fn speaks_babel(&self) -> bool {
        self.speaks_babel
    }

    #[must_use]
    pub const fn curve_scale(&self) -> f32 {
        self.curve_scale
    }

    #[must_use]
    pub const fn decision(&self) -> &SpeakingDecision {
        &self.decision
    }

    /// A per-speaker decision is stale independently of unrelated speakers.
    #[must_use]
    pub fn is_fresh(&self, ttl: Duration, now: Instant) -> bool {
        now.saturating_duration_since(self.published_at) <= ttl
    }
}

/// Latest validated pose of the local ACRE listener.
#[derive(Clone, Debug, PartialEq)]
pub struct AcreListenerSnapshot {
    generation: u64,
    sequence: u64,
    published_at: Instant,
    state: AcreListenerState,
}

impl AcreListenerSnapshot {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn published_at(&self) -> Instant {
        self.published_at
    }

    #[must_use]
    pub const fn pose(&self) -> crate::AcreListenerPose {
        self.state.pose
    }

    #[must_use]
    pub const fn curve_model(&self) -> AcreVoiceCurveModel {
        self.state.curve_model
    }

    #[must_use]
    pub const fn state(&self) -> AcreListenerState {
        self.state
    }

    #[must_use]
    pub fn is_fresh(&self, ttl: Duration, now: Instant) -> bool {
        now.saturating_duration_since(self.published_at) <= ttl
    }
}

/// One validated ACRE audio-plane update accepted by the control publisher.
#[derive(Clone, Debug, PartialEq)]
pub enum AcreAudioUpdate {
    Listener(AcreListenerState),
    Speaker(AcreSpeakerAudioUpdate),
}

/// One immutable generation of ACRE decisions suitable for atomic publication.
#[derive(Clone, Debug)]
pub struct AcreAudioSnapshot {
    generation: u64,
    sequence: u64,
    published_at: Instant,
    listener: Option<AcreListenerSnapshot>,
    speakers: HashMap<u32, AcreSpeakerSnapshot>,
}

impl AcreAudioSnapshot {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Monotonic sequence of the newest accepted decision in this generation.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn published_at(&self) -> Instant {
        self.published_at
    }

    #[must_use]
    pub const fn listener(&self) -> Option<&AcreListenerSnapshot> {
        self.listener.as_ref()
    }

    #[must_use]
    pub fn speaker(&self, speaker_id: u32) -> Option<&AcreSpeakerSnapshot> {
        self.speakers.get(&speaker_id)
    }

    #[must_use]
    pub fn speaker_count(&self) -> usize {
        self.speakers.len()
    }
}

/// Control-plane builder for immutable ACRE audio snapshots.
///
/// It only mutates after a valid update passes the capacity and sequence checks,
/// so callers can keep the last published snapshot until its TTL expires when
/// a malformed or over-capacity update is rejected.
#[derive(Debug)]
pub struct AcreAudioSnapshotBuilder {
    generation: u64,
    sequence: u64,
    max_speakers: usize,
    listener: Option<AcreListenerSnapshot>,
    speakers: HashMap<u32, AcreSpeakerSnapshot>,
}

impl Default for AcreAudioSnapshotBuilder {
    fn default() -> Self {
        Self::with_max_speakers(ACRE_AUDIO_MAX_SPEAKERS)
            .expect("the built-in ACRE audio speaker capacity is non-zero")
    }
}

impl AcreAudioSnapshotBuilder {
    /// Creates a builder with an explicit, non-zero capacity for tests or a
    /// future deployment-specific control-plane limit.
    pub fn with_max_speakers(max_speakers: usize) -> Result<Self, AcreAudioSnapshotError> {
        if max_speakers == 0 {
            return Err(AcreAudioSnapshotError::ZeroSpeakerCapacity);
        }
        Ok(Self {
            generation: 1,
            sequence: 0,
            max_speakers,
            listener: None,
            speakers: HashMap::with_capacity(max_speakers),
        })
    }

    /// Accepts one fully parsed ACRE decision and returns the next immutable
    /// snapshot. The caller provides a monotonic control-plane timestamp.
    pub fn apply(
        &mut self,
        update: SpeakingUpdate,
        published_at: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        self.apply_speaker(update.into(), published_at)
    }

    /// Accepts a remote decision paired with its validated peer curve scale.
    pub fn apply_speaker(
        &mut self,
        update: AcreSpeakerAudioUpdate,
        published_at: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        let is_known_speaker = self.speakers.contains_key(&update.speaking.speaker_id);
        if !is_known_speaker && self.speakers.len() >= self.max_speakers {
            return Err(AcreAudioSnapshotError::TooManySpeakers {
                limit: self.max_speakers,
            });
        }
        let sequence = self.next_sequence()?;
        let speaker = AcreSpeakerSnapshot {
            speaker_id: update.speaking.speaker_id,
            generation: self.generation,
            sequence,
            published_at,
            speaks_babel: update.speaking.speaks_babel,
            curve_scale: update.curve_scale,
            decision: update.speaking.decision,
        };
        self.speakers.insert(speaker.speaker_id, speaker);
        self.sequence = sequence;
        Ok(self.snapshot(published_at))
    }

    /// Accepts either local listener state or a remote speaker decision while
    /// preserving one sequence order for the full immutable generation.
    pub fn apply_update(
        &mut self,
        update: AcreAudioUpdate,
        published_at: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        match update {
            AcreAudioUpdate::Listener(state) => self.apply_listener(state, published_at),
            AcreAudioUpdate::Speaker(update) => self.apply_speaker(update, published_at),
        }
    }

    /// Publishes the local listener pose and curve model after parsing all
    /// coordinates outside the audio callback.
    pub fn apply_listener(
        &mut self,
        state: AcreListenerState,
        published_at: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        let sequence = self.next_sequence()?;
        self.listener = Some(AcreListenerSnapshot {
            generation: self.generation,
            sequence,
            published_at,
            state,
        });
        self.sequence = sequence;
        Ok(self.snapshot(published_at))
    }

    /// Frees a departed Mumble session's slot without invalidating other audio.
    pub fn remove_speaker(
        &mut self,
        speaker_id: u32,
        now: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        let sequence = self.next_sequence()?;
        self.speakers.remove(&speaker_id);
        self.sequence = sequence;
        Ok(self.snapshot(now))
    }

    /// Starts a new pipe/control generation and clears all previous decisions.
    /// A generation never silently wraps to zero.
    pub fn reset(
        &mut self,
        published_at: Instant,
    ) -> Result<AcreAudioSnapshot, AcreAudioSnapshotError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(AcreAudioSnapshotError::GenerationExhausted)?;
        self.generation = generation;
        self.sequence = 0;
        self.listener = None;
        self.speakers.clear();
        Ok(self.snapshot(published_at))
    }

    #[must_use]
    pub fn snapshot(&self, published_at: Instant) -> AcreAudioSnapshot {
        AcreAudioSnapshot {
            generation: self.generation,
            sequence: self.sequence,
            published_at,
            listener: self.listener.clone(),
            speakers: self.speakers.clone(),
        }
    }

    fn next_sequence(&self) -> Result<u64, AcreAudioSnapshotError> {
        self.sequence
            .checked_add(1)
            .ok_or(AcreAudioSnapshotError::SequenceExhausted)
    }
}

/// Rejection reason that leaves the previous immutable snapshot unchanged.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AcreAudioSnapshotError {
    #[error("ACRE audio snapshot capacity must be non-zero")]
    ZeroSpeakerCapacity,
    #[error("ACRE audio snapshot supports at most {limit} speakers")]
    TooManySpeakers { limit: usize },
    #[error("ACRE audio snapshot sequence is exhausted")]
    SequenceExhausted,
    #[error("ACRE audio snapshot generation is exhausted")]
    GenerationExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AcreListenerPose, AcreListenerState, AcreSpeakerVector, AcreVoiceCurveModel,
        RadioReceptionPath, SpatialSpeakingKind,
    };

    fn direct(speaker_id: u32, volume: f32) -> SpeakingUpdate {
        SpeakingUpdate {
            speaker_id,
            speaks_babel: false,
            decision: SpeakingDecision::Spatial {
                kind: SpatialSpeakingKind::Direct,
                volume,
                position: AcreSpeakerVector {
                    x: 10.0,
                    z: 30.0,
                    y: 20.0,
                },
                head_vector: AcreSpeakerVector {
                    x: 0.0,
                    z: 1.0,
                    y: 0.0,
                },
            },
        }
    }

    fn listener() -> AcreListenerState {
        AcreListenerState {
            pose: AcreListenerPose {
                position: AcreSpeakerVector {
                    x: 1.0,
                    z: 3.0,
                    y: 2.0,
                },
                head_vector: AcreSpeakerVector {
                    x: 0.0,
                    z: 1.0,
                    y: 0.0,
                },
            },
            curve_model: AcreVoiceCurveModel::SelectableB,
        }
    }

    #[test]
    fn preserves_session_generation_sequence_and_timestamp() {
        let at = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::default();
        let snapshot = builder.apply(direct(42, 0.75), at).unwrap();
        let speaker = snapshot.speaker(42).unwrap();

        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.sequence(), 1);
        assert_eq!(snapshot.published_at(), at);
        assert_eq!(speaker.speaker_id(), 42);
        assert_eq!(speaker.generation(), 1);
        assert_eq!(speaker.sequence(), 1);
        assert_eq!(speaker.published_at(), at);
        assert!(speaker.is_fresh(Duration::from_millis(1), at));
        assert!((speaker.curve_scale() - 1.0).abs() < f32::EPSILON);
        let SpeakingDecision::Spatial { kind, volume, .. } = speaker.decision() else {
            panic!("expected direct spatial decision");
        };
        assert_eq!(*kind, SpatialSpeakingKind::Direct);
        assert!((*volume - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn retains_the_listener_pose_with_the_same_generation_as_speakers() {
        let at = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::default();
        let listener_snapshot = builder
            .apply_update(AcreAudioUpdate::Listener(listener()), at)
            .unwrap();
        let snapshot = builder
            .apply_update(AcreAudioUpdate::Speaker(direct(42, 0.75).into()), at)
            .unwrap();

        let listener_state = listener_snapshot.listener().unwrap();
        assert_eq!(listener_state.generation(), 1);
        assert_eq!(listener_state.sequence(), 1);
        assert_eq!(listener_state.state(), listener());
        assert_eq!(
            listener_state.curve_model(),
            AcreVoiceCurveModel::SelectableB
        );
        assert_eq!(snapshot.generation(), 1);
        assert_eq!(snapshot.sequence(), 2);
        assert_eq!(snapshot.listener().unwrap().sequence(), 1);
        assert_eq!(snapshot.speaker(42).unwrap().sequence(), 2);
    }

    #[test]
    fn capacity_rejection_preserves_the_last_valid_snapshot() {
        let at = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::with_max_speakers(1).unwrap();
        let valid = builder.apply(direct(1, 0.5), at).unwrap();

        assert!(matches!(
            builder.apply(direct(2, 0.6), at),
            Err(AcreAudioSnapshotError::TooManySpeakers { limit: 1 })
        ));
        let retained = builder.snapshot(at);
        assert_eq!(retained.sequence(), valid.sequence());
        assert!(retained.speaker(1).is_some());
        assert!(retained.speaker(2).is_none());
    }

    #[test]
    fn reset_invalidates_every_prior_speaker_in_a_new_generation() {
        let at = Instant::now();
        let mut builder = AcreAudioSnapshotBuilder::default();
        builder.apply_listener(listener(), at).unwrap();
        builder.apply(direct(7, 1.0), at).unwrap();

        let reset = builder.reset(at).unwrap();
        assert_eq!(reset.generation(), 2);
        assert_eq!(reset.sequence(), 0);
        assert!(reset.listener().is_none());
        assert_eq!(reset.speaker_count(), 0);
    }

    #[test]
    fn preserves_every_radio_path_without_recomputing_acre_routing() {
        let at = Instant::now();
        let update = SpeakingUpdate {
            speaker_id: 8,
            speaks_babel: true,
            decision: SpeakingDecision::Radio {
                paths: vec![
                    RadioReceptionPath {
                        volume: 0.8,
                        signal_quality: 0.7,
                        signal_model: 2.0,
                        loudspeaker: false,
                        position: AcreSpeakerVector {
                            x: 1.0,
                            z: 3.0,
                            y: 2.0,
                        },
                    },
                    RadioReceptionPath {
                        volume: 0.4,
                        signal_quality: 0.9,
                        signal_model: 3.0,
                        loudspeaker: true,
                        position: AcreSpeakerVector {
                            x: 4.0,
                            z: 6.0,
                            y: 5.0,
                        },
                    },
                ],
            },
        };
        let mut builder = AcreAudioSnapshotBuilder::default();
        let snapshot = builder.apply(update, at).unwrap();
        let speaker = snapshot.speaker(8).unwrap();

        assert!(speaker.speaks_babel());
        let SpeakingDecision::Radio { paths } = speaker.decision() else {
            panic!("expected radio decision");
        };
        assert_eq!(paths.len(), 2);
        assert!((paths[0].volume - 0.8).abs() < f32::EPSILON);
        assert!(paths[1].loudspeaker);
        assert!((paths[1].position.y - 5.0).abs() < f32::EPSILON);
    }
}
