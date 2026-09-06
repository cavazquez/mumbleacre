//! Windows main-thread timer dispatches Mumble APIs; workers only own pipes/files.
use crate::timer::Timer;
use crate::{
    acre_adapter::{AcreAdapterEvent, AcreAdapterRuntime},
    audio,
    control::*,
    ffi,
};
use mumbleacre_acre::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub enum Input {
    Peer(i32, u32, Vec<u8>),
}
static INPUT: Mutex<VecDeque<Input>> = Mutex::new(VecDeque::new());
static OVERFLOW: AtomicBool = AtomicBool::new(false);
static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

// Peer messages are additionally accepted only from users in the active
// Mumble channel. An explicit value remains available for deployments that
// want to isolate a group beyond that channel boundary.
const DEFAULT_SCOPE: &str = "mumble-channel";

struct Runtime {
    adapter: AcreAdapterRuntime,
    timer: Option<Timer>,
    scope: String,
    generation: u64,
    sequence: u64,
    context: Option<ffi::Context>,
    peers: RemotePeers,
    ptt: AcrePttStateMachine,
    direct: Option<Transmission>,
    active: Option<Transmission>,
    native_talking: bool,
    connected: bool,
    last_send: Instant,
    dirty: bool,
    mic: bool,
    sound_system_override: bool,
    locally_muted: bool,
}
pub fn enqueue(input: Input) {
    let mut queue = INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if queue.len() < 256 {
        queue.push_back(input);
    } else {
        OVERFLOW.store(true, Ordering::Release);
    }
}
pub fn start() -> Result<(), String> {
    let scope = std::env::var("MUMBLEACRE_MISSION").unwrap_or_else(|_| DEFAULT_SCOPE.to_owned());
    PeerHeader::new(&scope, 1, 1).map_err(|e| e.to_string())?;
    let mut runtime = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runtime.is_some() {
        return Err("backend already started".into());
    }
    let log_path = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MumbleACRE")
        .join("logs")
        .join("plugin.log");
    let log =
        mumbleacre_logging::EventLog::open(log_path, "plugin", mumbleacre_logging::LogLevel::Info)
            .map(|log| Arc::new(Mutex::new(log)))
            .map_err(|e| format!("could not open diagnostic log: {e}"))?;
    let adapter = AcreAdapterRuntime::start(Some(log))?;
    INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    OVERFLOW.store(false, Ordering::Release);
    let generation = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_micros(),
    )
    .map_err(|e| e.to_string())?;
    let timer = Some(Timer::start()?);
    *runtime = Some(Runtime {
        adapter,
        timer,
        scope,
        generation,
        sequence: 0,
        context: None,
        peers: Default::default(),
        ptt: Default::default(),
        direct: None,
        active: None,
        native_talking: false,
        connected: false,
        last_send: Instant::now(),
        dirty: true,
        mic: false,
        sound_system_override: false,
        locally_muted: false,
    });
    drop(runtime);
    ffi::log(
        "MumbleACRE: esperando ACRE2; los pares se limitan al canal Mumble activo; servidor requiere pluginmessagelimit=20 y pluginmessageburst=40",
    );
    Ok(())
}
pub fn stop() {
    let runtime = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(mut r) = runtime {
        drop(r.timer.take());
        let direct = r.direct.clone();
        r.reset();
        r.direct = direct;
        r.send_state();
        r.adapter.stop();
    }
    INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    let _ = audio::get().decisions.reset();
    audio::get().peers.store(Arc::new(Default::default()));
}
pub(crate) fn tick() {
    // Take ownership before invoking any API: callbacks may reenter enqueue.
    let Some(mut runtime) = RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return;
    };
    runtime.tick();
    *RUNTIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(runtime);
}
impl Runtime {
    fn reset(&mut self) {
        crate::capture::close();
        let actions = self.ptt.reset();
        self.ptt_actions(actions);
        self.native_talking = false;
        crate::capture::clear_native();
        self.active = None;
        self.direct = None;
        let actions = self
            .peers
            .ids()
            .into_iter()
            .flat_map(|id| self.peers.remove(id))
            .collect();
        if self.adapter.enqueue_peer_actions(actions).is_err() {
            self.adapter.request_reset();
        }
        self.peers = RemotePeers::default();
        self.generation = self.generation.saturating_add(1);
        self.sequence = 0;
        self.dirty = true;
        self.sound_system_override = false;
        self.locally_muted = false;
        audio::set_sound_system_override(false);
        if ffi::microphone(false) {
            self.mic = false;
        }
        let _ = audio::get().decisions.reset();
        audio::get().peers.store(Arc::new(Default::default()));
    }

    fn voice_is_suppressed(&self) -> bool {
        self.sound_system_override || self.locally_muted
    }

    fn suppress_local_microphone(&mut self) {
        crate::capture::close();
        if ffi::microphone(false) {
            self.mic = false;
        }
    }
    fn ptt_actions(&mut self, actions: Vec<PttAction>) {
        for action in actions {
            match action {
                PttAction::PeerStarted(t) => {
                    crate::capture::close();
                    if t.speaking_kind() == SpeakingKind::Direct {
                        self.adapter.notify_direct(&t, true);
                    }
                    self.active = Some(t);
                    self.dirty = true;
                }
                PttAction::PeerStopped(t) => {
                    crate::capture::close();
                    if t.speaking_kind() == SpeakingKind::Direct {
                        self.adapter.notify_direct(&t, false);
                    }
                    self.active = None;
                    self.dirty = true;
                }
                PttAction::SetMicrophoneActivationOverwrite(active) => {
                    // Activation is deferred until the corresponding state is sent.
                    if !active && ffi::microphone(false) {
                        self.mic = false;
                    }
                }
            }
        }
    }
    fn peer_actions(&mut self, actions: Vec<PeerStateAction>) {
        for action in &actions {
            match action {
                PeerStateAction::TransmissionStarted(t)
                | PeerStateAction::TransmissionStopped(t) => audio::mute(t.voice_client_id()),
                PeerStateAction::Reset { voice_client_id } => {
                    audio::get().decisions.remove(*voice_client_id)
                }
                _ => {}
            }
        }
        if self.adapter.enqueue_peer_actions(actions).is_err() {
            self.reset();
            self.adapter.request_reset();
            ffi::log("MumbleACRE: cola ACRE saturada; reconectando");
        }
    }
    fn refresh_direct(&mut self) {
        let direct = if self.connected {
            self.adapter.direct()
        } else {
            None
        };
        if self.direct.as_ref().map(Transmission::net_id)
            != direct.as_ref().map(Transmission::net_id)
        {
            self.reset();
            self.dirty = true;
        }
        self.direct = direct;
    }
    fn tick(&mut self) {
        let now = Instant::now();
        if self.mic
            && (self.voice_is_suppressed() || !self.ptt.microphone_overwrite_active())
            && ffi::microphone(false)
        {
            self.mic = false;
        }
        let context = ffi::context();
        let changed = match (&self.context, &context) {
            (Some(a), Some(b)) => {
                a.connection != b.connection
                    || a.local != b.local
                    || a.channel != b.channel
                    || a.server != b.server
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.reset();
            self.connected = false;
            self.adapter.request_reset();
        }
        self.context = context;
        if let Some(ctx) = &self.context {
            self.adapter.set_local_voice_client_id(Some(ctx.local));
            fn ascii(s: &str) -> String {
                s.chars()
                    .take(128)
                    .map(|c| {
                        if c.is_ascii() && !c.is_ascii_control() && c != ',' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect()
            }
            self.adapter.set_voip_metadata(
                VoipMetadata::new(
                    "Mumble",
                    ascii(&ctx.server),
                    ascii(&ctx.name),
                    ctx.channel.to_string(),
                )
                .ok(),
            );
            let removed = self
                .peers
                .ids()
                .into_iter()
                .filter(|id| !ctx.users.contains(id))
                .collect::<Vec<_>>();
            for id in removed {
                let actions = self.peers.remove(id);
                self.peer_actions(actions);
            }
        } else {
            self.adapter.set_local_voice_client_id(None);
            self.adapter.set_voip_metadata(None);
        }
        if OVERFLOW.swap(false, Ordering::AcqRel) {
            self.reset();
            self.adapter.request_reset();
        }
        // Publish older pipe decisions before applying transitions, which mute
        // old audio immediately while ACRE evaluates the replacement mode.
        let audible = self.peers.audible(now);
        for update in self.adapter.drain_audio_updates() {
            if let AcreAudioUpdate::Speaker(speaker) = &update
                && !audible.contains_key(&speaker.speaking.speaker_id)
            {
                continue;
            }
            let _ = audio::get().decisions.publish(update);
        }
        for event in self.adapter.drain_events() {
            match event {
                AcreAdapterEvent::PipeConnected => {
                    self.connected = true;
                    ffi::log("MumbleACRE: ACRE conectado");
                }
                AcreAdapterEvent::PipeDisconnected => {
                    self.reset();
                    self.connected = false;
                }
                AcreAdapterEvent::ResetRequested => self.reset(),
                AcreAdapterEvent::QueueOverflow => {
                    self.reset();
                    self.connected = false;
                    self.adapter.request_reset();
                }
                AcreAdapterEvent::LocalTransmissionStarted(t) => {
                    self.refresh_direct();
                    if self
                        .context
                        .as_ref()
                        .is_some_and(|c| c.local == t.voice_client_id())
                        && !matches!(t.speaking_kind(), SpeakingKind::God | SpeakingKind::Zeus)
                        && let Ok(actions) = self.ptt.acre_started(t)
                    {
                        self.ptt_actions(actions);
                    }
                }
                AcreAdapterEvent::LocalTransmissionStopped(t) => {
                    if self.ptt.active_acre() == Some(&t) {
                        crate::capture::clear_native();
                        let actions = self.ptt.native_stopped();
                        self.ptt_actions(actions);
                    }
                    let actions = self.ptt.acre_stopped(&t);
                    self.ptt_actions(actions);
                }
                AcreAdapterEvent::SoundSystemOverrideChanged(enabled) => {
                    self.sound_system_override = enabled;
                    audio::set_sound_system_override(enabled);
                    if enabled {
                        self.suppress_local_microphone();
                    }
                    self.dirty = true;
                }
                AcreAdapterEvent::LocalMuteChanged(muted) => {
                    self.locally_muted = muted;
                    if muted {
                        self.suppress_local_microphone();
                    }
                    self.dirty = true;
                }
                AcreAdapterEvent::UserMuteChanged {
                    voice_client_id,
                    muted,
                } => {
                    if let Some(context) = self.context.as_ref() {
                        let _ = ffi::request_local_mute(context, voice_client_id, muted);
                    }
                }
                AcreAdapterEvent::SoundPlaybackReady {
                    generation,
                    id,
                    path,
                } => {
                    if self.adapter.is_current_sound_generation(generation) && !ffi::play(&path) {
                        self.adapter.enqueue_sound_error(&id);
                    }
                }
                AcreAdapterEvent::RejectedMessage(_) | AcreAdapterEvent::PipeError(_) => {}
            }
        }
        self.refresh_direct();
        if self.connected && !self.adapter.healthy() {
            self.reset();
            self.connected = false;
            self.adapter.request_reset();
            ffi::log("MumbleACRE: ACRE dejó de responder; audio silenciado");
        }
        let inputs = std::mem::take(
            &mut *INPUT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for input in inputs {
            match input {
                Input::Peer(connection, sender, bytes) => {
                    if self.connected
                        && self.direct.is_some()
                        && self.context.as_ref().is_some_and(|c| {
                            c.connection == connection && c.users.contains(&sender)
                        })
                        && let Ok(state) = StateMessage::decode(&bytes)
                        && let Ok(actions) = self.peers.receive(sender, &self.scope, state, now)
                    {
                        self.peer_actions(actions);
                    }
                }
            }
        }
        self.native_talking = !self.voice_is_suppressed() && crate::capture::native();
        if let Some(direct) = self.direct.clone() {
            let actions = if self.native_talking {
                self.ptt.direct_started(direct).unwrap_or_default()
            } else {
                self.ptt.native_stopped()
            };
            self.ptt_actions(actions);
        }
        let expired = self.peers.expire(now);
        self.peer_actions(expired);
        audio::get().peers.store(Arc::new(self.peers.audible(now)));
        if (self.dirty && self.last_send.elapsed() >= std::time::Duration::from_millis(50))
            || self.last_send.elapsed() >= REFRESH
        {
            self.send_state();
        }
        if self.adapter.take_dropped_speaking_updates() > 0 {
            ffi::log("MumbleACRE: demasiados hablantes; se descartaron decisiones de audio");
        }
    }
    fn send_state(&mut self) {
        let (Some(ctx), Some(direct)) = (&self.context, &self.direct) else {
            return;
        };
        if !self.connected {
            return;
        }
        if self.voice_is_suppressed() {
            self.suppress_local_microphone();
            self.dirty = true;
            return;
        }
        self.sequence = self.sequence.saturating_add(1);
        let Ok(bytes) = StateMessage::new(
            &self.scope,
            self.generation,
            self.sequence,
            direct.net_id(),
            self.active.as_ref(),
        )
        .and_then(|m| m.encode()) else {
            return;
        };
        self.last_send = Instant::now();
        self.dirty = false;
        if ffi::send(ctx, &bytes) {
            if self.active.is_some() {
                crate::capture::renew();
            } else {
                crate::capture::close();
            }
            let want = self.ptt.microphone_overwrite_active();
            if want != self.mic && ffi::microphone(want) {
                self.mic = want;
            }
        } else {
            crate::capture::close();
            if ffi::microphone(false) {
                self.mic = false;
            }
            self.dirty = true;
        }
    }
}
