//! Windows worker host for ACRE2's private named-pipe boundary.
//!
//! It deliberately owns no Mumble API calls.  The control worker only moves
//! bounded ACRE RPC messages and emits state changes for a later main/control
//! plane.  In particular, it never runs in `mumble_onAudioSourceFetched`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mumbleacre_acre::pipe::{AcrePipeError, AcrePipePair};
use mumbleacre_acre::{
    ACRE_AUDIO_MAX_SPEAKERS, AcreAction, AcreAudioUpdate, AcreListenerState, AcreMessage,
    AcreSession, AcreSpeakerAudioUpdate, PeerStateAction, Transmission, VoipMetadata,
    apply_peer_state_actions,
};
use mumbleacre_logging::LogLevel;

use crate::acre_sounds::{AcreSoundWorker, AcreSoundWorkerClient, AcreSoundWorkerResult};

const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(2);
const MAX_QUEUED_EVENTS: usize = 256;
const MAX_QUEUED_CONTROL_REPLIES: usize = 256;
const MAX_QUEUED_PEER_ACTIONS: usize = 256;

pub(crate) struct AcreAdapterRuntime {
    local_voice_client_id: Arc<AtomicU32>,
    direct: Arc<Mutex<Option<Transmission>>>,
    reset: Arc<AtomicBool>,
    last_activity: Arc<Mutex<Instant>>,
    voip_metadata: Arc<Mutex<Option<VoipMetadata>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    sound_worker: Option<AcreSoundWorker>,
    sound_generation: Arc<AtomicU64>,
    events: Arc<Mutex<VecDeque<AcreAdapterEvent>>>,
    control_replies: Arc<Mutex<VecDeque<AcreMessage>>>,
    peer_actions: Arc<Mutex<VecDeque<PeerStateAction>>>,
    listener_state: Arc<Mutex<Option<AcreListenerState>>>,
    speaking_updates: Arc<Mutex<HashMap<u32, AcreSpeakerAudioUpdate>>>,
    dropped_speaking_updates: Arc<AtomicU64>,
}

#[derive(Debug)]
pub(crate) enum AcreAdapterEvent {
    PipeConnected,
    PipeDisconnected,
    QueueOverflow,
    LocalTransmissionStarted(Transmission),
    LocalTransmissionStopped(Transmission),
    SoundSystemOverrideChanged(bool),
    LocalMuteChanged(bool),
    UserMuteChanged {
        voice_client_id: u32,
        muted: bool,
    },
    ResetRequested,
    SoundPlaybackReady {
        generation: u64,
        id: String,
        path: PathBuf,
    },
    RejectedMessage(String),
    PipeError(String),
}

/// Shared ownership passed once into the pipe worker. Grouping it makes the
/// boundary explicit: every mutable control-plane hand-off is still bounded
/// and owned by this worker, never by the audio callback.
struct WorkerShared {
    stop: Arc<AtomicBool>,
    local_voice_client_id: Arc<AtomicU32>,
    direct: Arc<Mutex<Option<Transmission>>>,
    reset: Arc<AtomicBool>,
    last_activity: Arc<Mutex<Instant>>,
    voip_metadata: Arc<Mutex<Option<VoipMetadata>>>,
    events: Arc<Mutex<VecDeque<AcreAdapterEvent>>>,
    control_replies: Arc<Mutex<VecDeque<AcreMessage>>>,
    sound_worker: AcreSoundWorkerClient,
    sound_generation: Arc<AtomicU64>,
    peer_actions: Arc<Mutex<VecDeque<PeerStateAction>>>,
    listener_state: Arc<Mutex<Option<AcreListenerState>>>,
    speaking_updates: Arc<Mutex<HashMap<u32, AcreSpeakerAudioUpdate>>>,
    dropped_speaking_updates: Arc<AtomicU64>,
}

impl AcreAdapterRuntime {
    pub(crate) fn start(event_log: Option<crate::SharedLog>) -> Result<Self, String> {
        // Create synchronously so ACRE2Arma can find both names as soon as the
        // Mumble plugin reports successful initialization.
        let pipes = AcrePipePair::create().map_err(|error| error.to_string())?;
        let sound_worker = AcreSoundWorker::start()?;
        let local_voice_client_id = Arc::new(AtomicU32::new(0));
        let direct = Arc::new(Mutex::new(None));
        let reset = Arc::new(AtomicBool::new(false));
        let last_activity = Arc::new(Mutex::new(Instant::now()));
        let voip_metadata = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let control_replies = Arc::new(Mutex::new(VecDeque::new()));
        let sound_generation = Arc::new(AtomicU64::new(0));
        let peer_actions = Arc::new(Mutex::new(VecDeque::new()));
        let listener_state = Arc::new(Mutex::new(None));
        let speaking_updates =
            Arc::new(Mutex::new(HashMap::with_capacity(ACRE_AUDIO_MAX_SPEAKERS)));
        let dropped_speaking_updates = Arc::new(AtomicU64::new(0));

        let worker_shared = WorkerShared {
            stop: Arc::clone(&stop),
            local_voice_client_id: Arc::clone(&local_voice_client_id),
            direct: Arc::clone(&direct),
            reset: Arc::clone(&reset),
            last_activity: Arc::clone(&last_activity),
            voip_metadata: Arc::clone(&voip_metadata),
            events: Arc::clone(&events),
            control_replies: Arc::clone(&control_replies),
            sound_worker: sound_worker.client(),
            sound_generation: Arc::clone(&sound_generation),
            peer_actions: Arc::clone(&peer_actions),
            listener_state: Arc::clone(&listener_state),
            speaking_updates: Arc::clone(&speaking_updates),
            dropped_speaking_updates: Arc::clone(&dropped_speaking_updates),
        };
        let worker = thread::Builder::new()
            .name("mumbleacre-acre-pipe".to_owned())
            .spawn(move || worker_loop(pipes, worker_shared, event_log))
            .map_err(|error| format!("could not start ACRE pipe worker: {error}"))?;

        Ok(Self {
            local_voice_client_id,
            direct,
            reset,
            last_activity,
            voip_metadata,
            stop,
            worker: Some(worker),
            sound_worker: Some(sound_worker),
            sound_generation,
            events,
            control_replies,
            peer_actions,
            listener_state,
            speaking_updates,
            dropped_speaking_updates,
        })
    }

    pub(crate) fn direct(&self) -> Option<Transmission> {
        self.direct
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(crate) fn request_reset(&self) {
        *self
            .direct
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.reset.store(true, Ordering::Release);
    }
    pub(crate) fn healthy(&self) -> bool {
        self.last_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed()
            < Duration::from_secs(5)
    }
    pub(crate) fn notify_direct(&self, transmission: &Transmission, started: bool) -> bool {
        let args = if started {
            vec![
                transmission.voice_client_id().to_string(),
                transmission.language_id().to_string(),
                "0".into(),
                String::new(),
            ]
        } else {
            vec![
                transmission.voice_client_id().to_string(),
                "0".into(),
                String::new(),
            ]
        };
        let Ok(message) = AcreMessage::with_trailing_comma(
            if started {
                "localStartSpeaking"
            } else {
                "localStopSpeaking"
            },
            args,
            true,
        ) else {
            return false;
        };
        enqueue_control_reply(&self.control_replies, message)
    }

    /// `0` is only used internally as the unavailable sentinel; real Mumble
    /// IDs are supplied after `mumble_onServerSynchronized`.
    pub(crate) fn set_local_voice_client_id(&self, voice_client_id: Option<u32>) {
        self.local_voice_client_id
            .store(voice_client_id.unwrap_or(0), Ordering::Release);
    }

    /// Replaces metadata captured from a synchronized Mumble control callback.
    /// The pipe worker copies it before handling each RPC and never calls the
    /// Mumble API itself.
    pub(crate) fn set_voip_metadata(&self, metadata: Option<VoipMetadata>) {
        *self
            .voip_metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = metadata;
    }

    pub(crate) fn drain_events(&self) -> Vec<AcreAdapterEvent> {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        events.drain(..).collect()
    }

    /// Returns a `handleSoundError` RPC to ACRE after a Mumble plugin callback
    /// had `playSample` reject a prepared local sound. The audio callback never
    /// participates in this queue.
    pub(crate) fn enqueue_sound_error(&self, id: &str) -> bool {
        let Ok(message) = sound_error_reply(id) else {
            return false;
        };
        enqueue_control_reply(&self.control_replies, message)
    }

    #[must_use]
    pub(crate) fn is_current_sound_generation(&self, generation: u64) -> bool {
        self.sound_generation.load(Ordering::Acquire) == generation
    }

    /// Takes the newest listener pose and parsed decision per speaker. The
    /// worker coalesces high-frequency ACRE updates before this control-plane
    /// hand-off; it never publishes audio from the pipe thread.
    pub(crate) fn drain_audio_updates(&self) -> Vec<AcreAudioUpdate> {
        let listener = self
            .listener_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let mut updates = self
            .speaking_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, update)| update)
            .collect::<Vec<_>>();
        updates.sort_unstable_by_key(|update| update.speaking.speaker_id);
        let mut audio_updates = Vec::with_capacity(updates.len() + usize::from(listener.is_some()));
        if let Some(listener) = listener {
            audio_updates.push(AcreAudioUpdate::Listener(listener));
        }
        audio_updates.extend(updates.into_iter().map(AcreAudioUpdate::Speaker));
        audio_updates
    }

    /// Returns and clears the count of updates rejected because the bounded
    /// per-speaker cache was full.
    pub(crate) fn take_dropped_speaking_updates(&self) -> u64 {
        self.dropped_speaking_updates.swap(0, Ordering::Relaxed)
    }

    /// Queues peer state already authenticated and sequenced by the Mumble
    /// control plane. Only the pipe worker applies it to `AcreSession`, so no
    /// Mumble callback can write to an ACRE pipe directly.
    pub(crate) fn enqueue_peer_actions(
        &self,
        actions: Vec<PeerStateAction>,
    ) -> Result<(), &'static str> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut queued = self
            .peer_actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if actions.len() > MAX_QUEUED_PEER_ACTIONS
            || queued.len().saturating_add(actions.len()) > MAX_QUEUED_PEER_ACTIONS
        {
            return Err("ACRE peer-action queue is full");
        }
        queued.extend(actions);
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.peer_actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        clear_listener_state(&self.listener_state);
        self.speaking_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(mut sound_worker) = self.sound_worker.take() {
            sound_worker.stop();
        }
        self.control_replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl Drop for AcreAdapterRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

// The spawned worker owns these values for its full lifetime. Keeping the
// connect/read/reset sequence together makes the fail-closed ordering auditable.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn worker_loop(mut pipes: AcrePipePair, shared: WorkerShared, event_log: Option<crate::SharedLog>) {
    let started_at = Instant::now();
    let mut session = AcreSession::default();
    let mut outbound = VecDeque::<AcreMessage>::new();
    let mut was_connected = false;
    let mut sound_generation = 0_u64;

    while !shared.stop.load(Ordering::Acquire) {
        if shared.reset.swap(false, Ordering::AcqRel) {
            reset_after_disconnect(
                &mut pipes,
                &mut session,
                &mut outbound,
                &mut sound_generation,
                &shared,
                event_log.as_ref(),
            );
            was_connected = false;
        }
        if outbound.len() > MAX_QUEUED_CONTROL_REPLIES {
            reset_after_disconnect(
                &mut pipes,
                &mut session,
                &mut outbound,
                &mut sound_generation,
                &shared,
                event_log.as_ref(),
            );
            was_connected = false;
        }
        match pipes.try_connect() {
            Ok(connected) => {
                if connected && !was_connected {
                    push_event(
                        &shared.events,
                        event_log.as_ref(),
                        AcreAdapterEvent::PipeConnected,
                    );
                }
                if !connected && was_connected {
                    reset_after_disconnect(
                        &mut pipes,
                        &mut session,
                        &mut outbound,
                        &mut sound_generation,
                        &shared,
                        event_log.as_ref(),
                    );
                }
                was_connected = connected;
            }
            Err(error) => {
                push_event(
                    &shared.events,
                    event_log.as_ref(),
                    AcreAdapterEvent::PipeError(error.to_string()),
                );
                reset_after_disconnect(
                    &mut pipes,
                    &mut session,
                    &mut outbound,
                    &mut sound_generation,
                    &shared,
                    event_log.as_ref(),
                );
                was_connected = false;
                thread::sleep(CONTROL_POLL_INTERVAL);
                continue;
            }
        }

        if !was_connected {
            thread::sleep(CONTROL_POLL_INTERVAL);
            continue;
        }

        expire_sound_loads(&mut session, event_log.as_ref());
        drain_control_replies(&mut outbound, &shared.control_replies);
        drain_sound_worker_results(&mut outbound, &shared, sound_generation, event_log.as_ref());
        dispatch_queued_peer_actions(
            &mut session,
            &mut outbound,
            &mut sound_generation,
            &shared,
            event_log.as_ref(),
        );

        for _ in 0..MAX_QUEUED_CONTROL_REPLIES {
            if outbound.len() >= MAX_QUEUED_CONTROL_REPLIES {
                break;
            }
            match pipes.try_read_from_arma() {
                Ok(Some(message)) => {
                    let local_voice_client_id =
                        shared.local_voice_client_id.load(Ordering::Acquire);
                    session.set_local_voice_client_id(
                        (local_voice_client_id != 0).then_some(local_voice_client_id),
                    );
                    session.set_voip_metadata(
                        shared
                            .voip_metadata
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone(),
                    );
                    match session.handle_from_arma(&message, started_at.elapsed().as_secs_f64()) {
                        Ok(actions) => {
                            *shared
                                .last_activity
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Instant::now();
                            *shared
                                .direct
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                session.direct_transmission();
                            dispatch_actions(
                                actions,
                                &mut outbound,
                                &mut sound_generation,
                                &shared,
                                event_log.as_ref(),
                            );
                        }
                        Err(error) => {
                            push_event(
                                &shared.events,
                                event_log.as_ref(),
                                AcreAdapterEvent::RejectedMessage(error.to_string()),
                            );
                        }
                    }
                }
                Ok(None) => break,
                Err(AcrePipeError::Disconnected { .. } | AcrePipeError::NotConnected { .. }) => {
                    reset_after_disconnect(
                        &mut pipes,
                        &mut session,
                        &mut outbound,
                        &mut sound_generation,
                        &shared,
                        event_log.as_ref(),
                    );
                    was_connected = false;
                    break;
                }
                Err(error) => {
                    // A malformed frame was already consumed by the pipe. Keep
                    // the connection alive but report it without logging its
                    // potentially sensitive raw contents.
                    push_event(
                        &shared.events,
                        event_log.as_ref(),
                        AcreAdapterEvent::PipeError(error.to_string()),
                    );
                    break;
                }
            }
        }

        expire_sound_loads(&mut session, event_log.as_ref());
        drain_control_replies(&mut outbound, &shared.control_replies);
        drain_sound_worker_results(&mut outbound, &shared, sound_generation, event_log.as_ref());

        if was_connected
            && !flush_outbound(
                &mut pipes,
                &mut outbound,
                &shared.events,
                event_log.as_ref(),
            )
        {
            reset_after_disconnect(
                &mut pipes,
                &mut session,
                &mut outbound,
                &mut sound_generation,
                &shared,
                event_log.as_ref(),
            );
            was_connected = false;
        }
        thread::sleep(CONTROL_POLL_INTERVAL);
    }
    pipes.disconnect();
}

fn dispatch_actions(
    actions: Vec<AcreAction>,
    outbound: &mut VecDeque<AcreMessage>,
    sound_generation: &mut u64,
    shared: &WorkerShared,
    event_log: Option<&crate::SharedLog>,
) {
    for action in actions {
        match action {
            AcreAction::SendToArma(message) => outbound.push_back(message),
            AcreAction::LocalTransmissionStarted(transmission) => {
                crate::capture::close();
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::LocalTransmissionStarted(transmission),
                );
            }
            AcreAction::LocalTransmissionStopped(transmission) => {
                crate::capture::close();
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::LocalTransmissionStopped(transmission),
                );
            }
            AcreAction::SoundSystemOverrideChanged(enabled) => {
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::SoundSystemOverrideChanged(enabled),
                );
            }
            AcreAction::LocalMuteChanged(muted) => {
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::LocalMuteChanged(muted),
                );
            }
            AcreAction::UserMuteChanged {
                voice_client_id,
                muted,
            } => {
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::UserMuteChanged {
                        voice_client_id,
                        muted,
                    },
                );
            }
            AcreAction::ListenerUpdated(state) => {
                record_listener_state(state, &shared.listener_state);
            }
            AcreAction::SpeakingUpdated(update) => {
                record_speaking_update(
                    update,
                    &shared.speaking_updates,
                    &shared.dropped_speaking_updates,
                );
            }
            AcreAction::SoundLoaded(sound) => {
                let id = sound.id().to_owned();
                if let Err(reason) = shared.sound_worker.enqueue_load(*sound_generation, sound) {
                    outbound.push_back(loaded_sound_reply(&id, false));
                    crate::write_event(
                        event_log,
                        LogLevel::Warn,
                        "acre_sound_load_dropped",
                        &format!("id={id} reason={reason}"),
                    );
                }
            }
            AcreAction::SoundPlaybackRequested(request) => {
                let id = request.id().to_owned();
                if let Err(reason) = shared.sound_worker.enqueue_play(*sound_generation, request) {
                    outbound.push_back(sound_error_reply(&id).expect("validated ACRE sound ID"));
                    crate::write_event(
                        event_log,
                        LogLevel::Warn,
                        "acre_sound_playback_dropped",
                        &format!("id={id} reason={reason}"),
                    );
                }
            }
            AcreAction::ResetRequested => {
                outbound.clear();
                *shared
                    .direct
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                clear_queued_peer_actions(&shared.peer_actions);
                clear_listener_state(&shared.listener_state);
                clear_speaking_updates(&shared.speaking_updates);
                clear_control_replies(&shared.control_replies);
                clear_queued_sound_playbacks(&shared.events);
                reset_sound_worker(sound_generation, shared, event_log);
                push_event(&shared.events, event_log, AcreAdapterEvent::ResetRequested);
            }
        }
    }
}

fn dispatch_queued_peer_actions(
    session: &mut AcreSession,
    outbound: &mut VecDeque<AcreMessage>,
    sound_generation: &mut u64,
    shared: &WorkerShared,
    event_log: Option<&crate::SharedLog>,
) {
    let actions = {
        let mut queued = shared
            .peer_actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *queued)
    };
    if actions.is_empty() {
        return;
    }
    clear_speaking_updates(&shared.speaking_updates);
    match apply_peer_state_actions(session, actions) {
        Ok(actions) => dispatch_actions(actions, outbound, sound_generation, shared, event_log),
        Err(error) => push_event(
            &shared.events,
            event_log,
            AcreAdapterEvent::RejectedMessage(format!(
                "could not apply validated ACRE peer state: {error}"
            )),
        ),
    }
}

fn expire_sound_loads(session: &mut AcreSession, event_log: Option<&crate::SharedLog>) {
    for id in session.expire_sound_loads(Instant::now()) {
        crate::write_event(
            event_log,
            LogLevel::Warn,
            "acre_sound_load_expired",
            &format!("id={id}"),
        );
    }
}

fn enqueue_control_reply(
    replies: &Arc<Mutex<VecDeque<AcreMessage>>>,
    message: AcreMessage,
) -> bool {
    let mut replies = replies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if replies.len() >= MAX_QUEUED_CONTROL_REPLIES {
        return false;
    }
    replies.push_back(message);
    true
}

fn drain_control_replies(
    outbound: &mut VecDeque<AcreMessage>,
    replies: &Arc<Mutex<VecDeque<AcreMessage>>>,
) {
    let mut replies = replies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    outbound.append(&mut replies);
}

fn drain_sound_worker_results(
    outbound: &mut VecDeque<AcreMessage>,
    shared: &WorkerShared,
    sound_generation: u64,
    event_log: Option<&crate::SharedLog>,
) {
    for result in shared.sound_worker.drain_results() {
        match result {
            AcreSoundWorkerResult::Loaded {
                generation,
                id,
                sample_count,
            } if generation == sound_generation => {
                outbound.push_back(loaded_sound_reply(&id, true));
                crate::write_event(
                    event_log,
                    LogLevel::Debug,
                    "acre_sound_prepared",
                    &format!("id={id} samples={sample_count}"),
                );
            }
            AcreSoundWorkerResult::LoadFailed {
                generation,
                id,
                reason,
            } if generation == sound_generation => {
                outbound.push_back(loaded_sound_reply(&id, false));
                crate::write_event(
                    event_log,
                    LogLevel::Warn,
                    "acre_sound_load_failed",
                    &format!("id={id} reason={reason}"),
                );
            }
            AcreSoundWorkerResult::PlaybackReady {
                generation,
                id,
                path,
            } if generation == sound_generation => {
                push_event(
                    &shared.events,
                    event_log,
                    AcreAdapterEvent::SoundPlaybackReady {
                        generation,
                        id,
                        path,
                    },
                );
            }
            AcreSoundWorkerResult::PlaybackFailed {
                generation,
                id,
                reason,
            } if generation == sound_generation => {
                outbound.push_back(sound_error_reply(&id).expect("validated ACRE sound ID"));
                crate::write_event(
                    event_log,
                    LogLevel::Warn,
                    "acre_sound_playback_failed",
                    &format!("id={id} reason={reason}"),
                );
            }
            _ => {}
        }
    }
}

fn reset_sound_worker(
    sound_generation: &mut u64,
    shared: &WorkerShared,
    event_log: Option<&crate::SharedLog>,
) {
    *sound_generation = sound_generation.wrapping_add(1);
    shared
        .sound_generation
        .store(*sound_generation, Ordering::Release);
    if let Err(reason) = shared.sound_worker.enqueue_clear() {
        crate::write_event(
            event_log,
            LogLevel::Warn,
            "acre_sound_clear_dropped",
            &format!("reason={reason}"),
        );
    }
}

fn loaded_sound_reply(id: &str, loaded: bool) -> AcreMessage {
    AcreMessage::new(
        "handleLoadedSound",
        vec![id.to_owned(), if loaded { "1" } else { "0" }.to_owned()],
    )
    .expect("validated ACRE sound ID and static reply are encodable")
}

fn sound_error_reply(id: &str) -> Result<AcreMessage, mumbleacre_acre::AcreCodecError> {
    AcreMessage::with_trailing_comma("handleSoundError", vec![id.to_owned()], true)
}

/// Returns `false` only when the connection is no longer usable.
fn flush_outbound(
    pipes: &mut AcrePipePair,
    outbound: &mut VecDeque<AcreMessage>,
    events: &Arc<Mutex<VecDeque<AcreAdapterEvent>>>,
    event_log: Option<&crate::SharedLog>,
) -> bool {
    while let Some(message) = outbound.front() {
        match pipes.write_to_arma(message) {
            Ok(()) => {
                let _ = outbound.pop_front();
            }
            Err(AcrePipeError::WouldBlock { .. }) => return true,
            Err(AcrePipeError::Disconnected { .. } | AcrePipeError::NotConnected { .. }) => {
                return false;
            }
            Err(error) => {
                push_event(
                    events,
                    event_log,
                    AcreAdapterEvent::PipeError(error.to_string()),
                );
                let _ = outbound.pop_front();
            }
        }
    }
    true
}

fn reset_after_disconnect(
    pipes: &mut AcrePipePair,
    session: &mut AcreSession,
    outbound: &mut VecDeque<AcreMessage>,
    sound_generation: &mut u64,
    shared: &WorkerShared,
    event_log: Option<&crate::SharedLog>,
) {
    pipes.disconnect();
    session.reset();
    *shared
        .direct
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    outbound.clear();
    clear_queued_peer_actions(&shared.peer_actions);
    clear_listener_state(&shared.listener_state);
    clear_speaking_updates(&shared.speaking_updates);
    clear_control_replies(&shared.control_replies);
    clear_queued_sound_playbacks(&shared.events);
    reset_sound_worker(sound_generation, shared, event_log);
    push_event(
        &shared.events,
        event_log,
        AcreAdapterEvent::PipeDisconnected,
    );
}

fn clear_queued_peer_actions(peer_actions: &Arc<Mutex<VecDeque<PeerStateAction>>>) {
    peer_actions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn clear_control_replies(replies: &Arc<Mutex<VecDeque<AcreMessage>>>) {
    replies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn clear_queued_sound_playbacks(events: &Arc<Mutex<VecDeque<AcreAdapterEvent>>>) {
    events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|event| !matches!(event, AcreAdapterEvent::SoundPlaybackReady { .. }));
}

fn record_listener_state(
    state: AcreListenerState,
    listener_state: &Arc<Mutex<Option<AcreListenerState>>>,
) {
    *listener_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state);
}

fn clear_listener_state(listener_state: &Arc<Mutex<Option<AcreListenerState>>>) {
    *listener_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

fn record_speaking_update(
    update: AcreSpeakerAudioUpdate,
    speaking_updates: &Arc<Mutex<HashMap<u32, AcreSpeakerAudioUpdate>>>,
    dropped_speaking_updates: &AtomicU64,
) {
    let mut updates = speaking_updates
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !updates.contains_key(&update.speaking.speaker_id)
        && updates.len() >= ACRE_AUDIO_MAX_SPEAKERS
    {
        dropped_speaking_updates.fetch_add(1, Ordering::Relaxed);
        return;
    }
    updates.insert(update.speaking.speaker_id, update);
}

fn clear_speaking_updates(speaking_updates: &Arc<Mutex<HashMap<u32, AcreSpeakerAudioUpdate>>>) {
    speaking_updates
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn push_event(
    events: &Arc<Mutex<VecDeque<AcreAdapterEvent>>>,
    event_log: Option<&crate::SharedLog>,
    event: AcreAdapterEvent,
) {
    let (level, name, message) = match &event {
        AcreAdapterEvent::PipeConnected => (
            LogLevel::Info,
            "acre_pipe_connected",
            "ACRE2Arma connected to both named pipes".to_owned(),
        ),
        AcreAdapterEvent::QueueOverflow => (
            LogLevel::Warn,
            "acre_queue_overflow",
            "Control event queue overflow; reconnect required".into(),
        ),
        AcreAdapterEvent::PipeDisconnected => (
            LogLevel::Warn,
            "acre_pipe_disconnected",
            "ACRE2Arma disconnected; ACRE state was cleared".to_owned(),
        ),
        AcreAdapterEvent::LocalTransmissionStarted(transmission) => (
            LogLevel::Info,
            "acre_local_transmission_started",
            format!("speaking_kind={:?}", transmission.speaking_kind()),
        ),
        AcreAdapterEvent::LocalTransmissionStopped(transmission) => (
            LogLevel::Info,
            "acre_local_transmission_stopped",
            format!("speaking_kind={:?}", transmission.speaking_kind()),
        ),
        AcreAdapterEvent::SoundSystemOverrideChanged(enabled) => (
            LogLevel::Info,
            "acre_sound_system_override",
            format!("enabled={enabled}"),
        ),
        AcreAdapterEvent::LocalMuteChanged(muted) => {
            (LogLevel::Info, "acre_local_mute", format!("muted={muted}"))
        }
        AcreAdapterEvent::UserMuteChanged {
            voice_client_id,
            muted,
        } => (
            LogLevel::Debug,
            "acre_user_mute",
            format!("voice_client_id={voice_client_id} muted={muted}"),
        ),
        AcreAdapterEvent::ResetRequested => (
            LogLevel::Info,
            "acre_reset",
            "ACRE requested a control-state reset".to_owned(),
        ),
        AcreAdapterEvent::SoundPlaybackReady { id, .. } => (
            LogLevel::Debug,
            "acre_sound_playback_ready",
            format!("id={id}"),
        ),
        AcreAdapterEvent::RejectedMessage(error) => {
            (LogLevel::Warn, "acre_rpc_rejected", error.clone())
        }
        AcreAdapterEvent::PipeError(error) => (LogLevel::Warn, "acre_pipe_error", error.clone()),
    };
    crate::write_event(event_log, level, name, &message);

    let mut queue = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if queue.len() >= MAX_QUEUED_EVENTS {
        queue.clear();
        queue.push_back(AcreAdapterEvent::QueueOverflow);
    }
    queue.push_back(event);
}
