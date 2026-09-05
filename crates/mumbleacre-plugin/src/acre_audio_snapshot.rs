//! Atomic publication of parsed ACRE audio decisions.
//!
//! The named-pipe worker and Mumble callbacks are control-plane code. They
//! publish complete immutable generations here; a later AM-12/13 audio path
//! can load them without touching the global plugin mutex or rebuilding a
//! decision from radio frequencies.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::publication::Publication;
use mumbleacre_acre::{
    AcreAudioSnapshot, AcreAudioSnapshotBuilder, AcreAudioSnapshotError, AcreAudioUpdate,
};

pub(crate) struct AcreAudioSnapshotStore {
    builder: Mutex<AcreAudioSnapshotBuilder>,
    snapshot: Publication<AcreAudioSnapshot>,
}

impl Default for AcreAudioSnapshotStore {
    fn default() -> Self {
        let now = Instant::now();
        let builder = AcreAudioSnapshotBuilder::default();
        let snapshot = builder.snapshot(now);
        Self {
            builder: Mutex::new(builder),
            snapshot: Publication::new(snapshot),
        }
    }
}

impl AcreAudioSnapshotStore {
    /// Publishes one fully parsed ACRE decision. A rejection leaves the last
    /// immutable snapshot untouched so its TTL can fail closed at the reader.
    pub(crate) fn publish(&self, update: AcreAudioUpdate) -> Result<(), AcreAudioSnapshotError> {
        let snapshot = self
            .builder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_update(update, Instant::now())?;
        self.snapshot.store(Arc::new(snapshot));
        Ok(())
    }

    pub(crate) fn remove(&self, speaker_id: u32) {
        let snapshot = self
            .builder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove_speaker(speaker_id, Instant::now());
        if let Ok(snapshot) = snapshot {
            self.snapshot.store(Arc::new(snapshot));
        }
    }

    /// Starts a new generation and atomically removes every prior decision.
    pub(crate) fn reset(&self) -> Result<(), AcreAudioSnapshotError> {
        let snapshot = self
            .builder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reset(Instant::now())?;
        self.snapshot.store(Arc::new(snapshot));
        Ok(())
    }

    /// RT reader for the immutable publication boundary. The callback obtains
    /// only this `Arc`; it never locks the builder or reconstructs an ACRE
    /// decision from legacy state.
    #[must_use]
    pub(crate) fn load_full(&self) -> Arc<AcreAudioSnapshot> {
        self.snapshot.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{
        AcreListenerPose, AcreListenerState, AcreSpeakerVector, AcreVoiceCurveModel,
        SpatialSpeakingKind, SpeakingDecision, SpeakingUpdate,
    };

    fn update(speaker_id: u32) -> SpeakingUpdate {
        SpeakingUpdate {
            speaker_id,
            speaks_babel: false,
            decision: SpeakingDecision::Spatial {
                kind: SpatialSpeakingKind::Direct,
                volume: 0.5,
                position: AcreSpeakerVector {
                    x: 0.0,
                    z: 0.0,
                    y: 0.0,
                },
                head_vector: AcreSpeakerVector {
                    x: 0.0,
                    z: 1.0,
                    y: 0.0,
                },
            },
        }
    }

    #[test]
    fn publishes_complete_snapshots_and_resets_generation() {
        let store = AcreAudioSnapshotStore::default();
        let initial = store.load_full();
        assert_eq!(initial.generation(), 1);
        assert_eq!(initial.speaker_count(), 0);

        store
            .publish(AcreAudioUpdate::Speaker(update(44).into()))
            .unwrap();
        let published = store.load_full();
        assert_eq!(published.generation(), 1);
        assert_eq!(published.sequence(), 1);
        assert!(published.speaker(44).is_some());

        store
            .publish(AcreAudioUpdate::Listener(AcreListenerState {
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
                curve_model: AcreVoiceCurveModel::Original,
            }))
            .unwrap();
        assert!(store.load_full().listener().is_some());

        store.reset().unwrap();
        let reset = store.load_full();
        assert_eq!(reset.generation(), 2);
        assert_eq!(reset.speaker_count(), 0);
    }
}
