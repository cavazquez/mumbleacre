//! Compatibility boundary for ACRE2's private voice-plugin RPC.
//!
//! ACRE2 does not expose this protocol as a stable public API.  The adapter
//! therefore keeps its wire codec and the small control state machine here,
//! separated from Mumble FFI and from the real-time audio path.

mod audio_snapshot;
mod codec;
mod peer;
mod peer_dispatch;
mod peer_state;
mod procedure;
mod ptt;
mod session;
mod sound;
mod speaking;

#[cfg(windows)]
pub mod pipe;

pub use audio_snapshot::{
    ACRE_AUDIO_MAX_SPEAKERS, AcreAudioSnapshot, AcreAudioSnapshotBuilder, AcreAudioSnapshotError,
    AcreAudioUpdate, AcreListenerSnapshot, AcreSpeakerSnapshot,
};
pub use codec::{ACRE_MAX_MESSAGE_BYTES, ACRE_MAX_PARAMETER_COUNT, AcreCodecError, AcreMessage};
pub use peer::{
    ACRE_PEER_MAX_MESSAGE_BYTES, ACRE_PEER_PROTOCOL_VERSION, AcrePeerError, PeerEnvelope,
    PeerHeader, PeerOrderDecision, PeerOrderTracker, PeerPayload, PeerSpeakingKind,
};
pub use peer_dispatch::apply_peer_state_actions;
pub use peer_state::{PeerDirectory, PeerDirectoryError, PeerStateAction};
pub use procedure::{ACRE2_V214_WIRE_PROCEDURES, is_known_v214_procedure};
pub use ptt::{AcrePttError, AcrePttStateMachine, PttAction};
pub use session::{
    ACRE2_PLUGIN_VERSION, AcreAction, AcreSession, AcreSessionError, RemoteTransmission,
    SpeakingKind, Transmission, VoipMetadata,
};
pub use sound::{
    ACRE_MAX_PENDING_SOUNDS, ACRE_MAX_SOUND_CHUNKS, ACRE_MAX_SOUND_DECODED_BYTES,
    ACRE_MAX_SOUND_ENCODED_BYTES, ACRE_SOUND_LOAD_TIMEOUT, AcreLoadedSound, AcreSoundAssembler,
    AcreSoundError, AcreSoundLoadChunk, AcreSoundPlayback, parse_load_sound,
    parse_play_loaded_sound,
};
pub use speaking::{
    ACRE_MAX_RADIO_RECEPTION_PATHS, AcreListenerPose, AcreListenerState, AcreSpeakerAudioUpdate,
    AcreSpeakerVector, AcreSpeakingDataError, AcreVoiceCurveModel, RadioReceptionPath,
    SpatialSpeakingKind, SpeakingDecision, SpeakingUpdate, parse_update_speaking_data,
};
