use crate::publication::Publication;
use crate::{
    acre_audio_snapshot::AcreAudioSnapshotStore, acre_direct::*, acre_intercom::*, acre_monitor::*,
    acre_radio_render::*,
};
use mumbleacre_acre::{AcreAudioUpdate, SpatialSpeakingKind, SpeakingDecision};
use std::{
    cell::UnsafeCell,
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub struct Audio {
    pub decisions: AcreAudioSnapshotStore,
    pub peers: Publication<HashMap<u32, Instant>>,
    slots: Box<[Slot]>,
}
struct Slot {
    busy: AtomicBool,
    state: UnsafeCell<RenderState>,
}
// Access to the UnsafeCell is granted only by a successful atomic test-and-set.
// A competing/reentrant callback silences its buffer instead of waiting.
unsafe impl Sync for Slot {}
struct RenderState {
    id: u32,
    generation: u64,
    kind: u8,
    direct: AcreDirectGainState,
    radio: AcreRadioRenderState,
    intercom: AcreIntercomRenderState,
    monitor: AcreMonitorGainState,
}
impl RenderState {
    fn new(id: u32) -> Self {
        Self {
            id,
            generation: 0,
            kind: 0,
            direct: Default::default(),
            radio: AcreRadioRenderState::new(id),
            intercom: AcreIntercomRenderState::new(id),
            monitor: Default::default(),
        }
    }
    fn reset(&mut self) {
        self.direct.reset();
        self.radio.reset();
        self.intercom.reset();
        self.monitor.reset();
    }
}
static AUDIO: OnceLock<Audio> = OnceLock::new();
pub fn init() {
    AUDIO.get_or_init(|| Audio {
        decisions: Default::default(),
        peers: Publication::new(HashMap::new()),
        slots: (0..128)
            .map(|i| Slot {
                busy: AtomicBool::new(false),
                state: UnsafeCell::new(RenderState::new(i as u32)),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    });
}
pub fn get() -> &'static Audio {
    AUDIO.get().expect("audio initialized before runtime")
}
pub fn mute(id: u32) {
    let _ = get().decisions.publish(AcreAudioUpdate::Speaker(
        mumbleacre_acre::SpeakingUpdate {
            speaker_id: id,
            speaks_babel: false,
            decision: SpeakingDecision::Mute,
        }
        .into(),
    ));
}
pub fn process(id: u32, samples: &mut [f32], channels: usize, rate: u32) {
    let Some(audio) = AUDIO.get() else {
        samples.fill(0.0);
        return;
    };
    let now = Instant::now();
    let peers = audio.peers.load_full();
    if peers
        .get(&id)
        .is_none_or(|at| now.saturating_duration_since(*at) >= crate::control::LEASE)
    {
        samples.fill(0.0);
        return;
    }
    let snapshot = audio.decisions.load_full();
    let Some(speaker) = snapshot.speaker(id) else {
        samples.fill(0.0);
        return;
    };
    let slot = &audio.slots[id as usize % audio.slots.len()];
    if slot.busy.swap(true, Ordering::Acquire) {
        samples.fill(0.0);
        return;
    }
    struct Release<'a>(&'a AtomicBool);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(&slot.busy);
    // SAFETY: this callback exclusively owns this slot until Release drops.
    let state = unsafe { &mut *slot.state.get() };
    let kind = match speaker.decision() {
        SpeakingDecision::Spatial {
            kind: SpatialSpeakingKind::Intercom,
            ..
        } => 2,
        SpeakingDecision::Spatial { .. } => 1,
        SpeakingDecision::Radio { .. } => 3,
        SpeakingDecision::God { .. } | SpeakingDecision::Spectator { .. } => 4,
        _ => 0,
    };
    if state.id != id || state.generation != snapshot.generation() || state.kind != kind {
        state.reset();
        state.id = id;
        state.generation = snapshot.generation();
        state.kind = kind;
    }
    let modified = match kind {
        1 => {
            render_direct_with_gain_state(
                &snapshot,
                id,
                samples,
                channels,
                rate,
                now,
                &mut state.direct,
            ) == AcreDirectRenderAction::Modified
        }
        2 => {
            render_intercom_with_state(
                &snapshot,
                id,
                samples,
                channels,
                rate,
                now,
                &mut state.intercom,
            ) == AcreIntercomRenderAction::Modified
        }
        3 => {
            render_radio_with_state(
                &snapshot,
                id,
                samples,
                channels,
                rate,
                now,
                &mut state.radio,
            ) == AcreRadioRenderAction::Modified
        }
        4 => {
            render_monitor_with_gain_state(
                &snapshot,
                id,
                samples,
                channels,
                rate,
                now,
                &mut state.monitor,
            ) == AcreMonitorRenderAction::Modified
        }
        _ => false,
    };
    if !modified {
        samples.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::*;
    use std::{sync::Arc, time::Duration};
    #[test]
    fn integrated_audio_requires_peer_lease_and_acre_decision_without_allocating() {
        init();
        let audio = get();
        let id = 9001;
        let vector = AcreSpeakerVector {
            x: 0.0,
            z: 0.0,
            y: 0.0,
        };
        audio
            .decisions
            .publish(AcreAudioUpdate::Listener(AcreListenerState {
                pose: AcreListenerPose {
                    position: vector,
                    head_vector: AcreSpeakerVector {
                        x: 0.0,
                        z: 1.0,
                        y: 0.0,
                    },
                },
                curve_model: AcreVoiceCurveModel::Original,
            }))
            .unwrap();
        audio
            .decisions
            .publish(AcreAudioUpdate::Speaker(
                SpeakingUpdate {
                    speaker_id: id,
                    speaks_babel: false,
                    decision: SpeakingDecision::Spatial {
                        kind: SpatialSpeakingKind::Direct,
                        volume: 1.0,
                        position: vector,
                        head_vector: AcreSpeakerVector {
                            x: 0.0,
                            z: 1.0,
                            y: 0.0,
                        },
                    },
                }
                .into(),
            ))
            .unwrap();
        let mut pcm = [0.2; 960];
        process(id, &mut pcm, 2, 48000);
        assert!(pcm.iter().all(|v| *v == 0.0));
        audio
            .peers
            .store(Arc::new(HashMap::from([(id, Instant::now())])));
        pcm.fill(0.2);
        process(id, &mut pcm, 2, 48000);
        assert!(pcm.iter().any(|v| *v != 0.0));
        let (_, allocations) = crate::test_alloc::count_allocations(|| {
            pcm.fill(0.2);
            process(id, &mut pcm, 2, 48000);
        });
        assert_eq!(allocations, 0);
        audio.peers.store(Arc::new(HashMap::from([(
            id,
            Instant::now() - Duration::from_secs(1),
        )])));
        pcm.fill(0.2);
        process(id, &mut pcm, 2, 48000);
        assert!(pcm.iter().all(|v| *v == 0.0));
        audio
            .peers
            .store(Arc::new(HashMap::from([(id, Instant::now())])));
        mute(id);
        pcm.fill(0.2);
        process(id, &mut pcm, 2, 48000);
        assert!(pcm.iter().all(|v| *v == 0.0));
    }
}
