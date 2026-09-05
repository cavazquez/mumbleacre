//! Transport-independent peer identity and transmission state.
//!
//! The Mumble callback supplies the authoritative session ID. This module only
//! accepts a decoded [`crate::PeerEnvelope`] and produces control actions; it
//! neither calls Mumble nor writes to ACRE pipes.

use std::collections::HashMap;

use thiserror::Error;

use crate::{
    AcrePeerError, AcreSessionError, PeerEnvelope, PeerHeader, PeerOrderDecision, PeerOrderTracker,
    PeerPayload, PeerSpeakingKind, RemoteTransmission, SpeakingKind, Transmission,
};

/// An accepted peer-state transition for one authoritative Mumble sender.
#[derive(Clone, Debug, PartialEq)]
pub enum PeerStateAction {
    /// The sender has associated its Mumble session with this ACRE `netId`.
    Identity {
        voice_client_id: u32,
        net_id: String,
    },
    /// A remote ACRE speaking mode should be opened.
    TransmissionStarted(RemoteTransmission),
    /// A remote ACRE speaking mode should be closed.
    TransmissionStopped(RemoteTransmission),
    /// Caller must discard identity/control state for this sender.
    Reset { voice_client_id: u32 },
}

/// Receiver-side handshake, identity, order, and active-TX state.
#[derive(Clone, Debug)]
pub struct PeerDirectory {
    scope: String,
    peers: HashMap<u32, PeerState>,
}

#[derive(Clone, Debug, Default)]
struct PeerState {
    order: PeerOrderTracker,
    compatible: bool,
    net_id: Option<String>,
    active_transmission: Option<RemoteTransmission>,
}

impl PeerDirectory {
    /// Creates a directory limited to exactly one locally established mission
    /// scope. The scope comes from trusted local mission control, not from a
    /// peer's payload.
    pub fn new(scope: impl Into<String>) -> Result<Self, PeerDirectoryError> {
        let scope = scope.into();
        // Reuse the protocol's scope validation rather than maintaining a
        // second definition of valid scope syntax.
        let _ = PeerHeader::new(scope.clone(), 1, 1)?;
        Ok(Self {
            scope,
            peers: HashMap::new(),
        })
    }

    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Applies one envelope attributed to `voice_client_id` by Mumble.
    ///
    /// Semantic errors are transactional: invalid payloads do not consume a
    /// sequence number or mutate an existing peer state.
    pub fn apply(
        &mut self,
        voice_client_id: u32,
        envelope: &PeerEnvelope,
    ) -> Result<Vec<PeerStateAction>, PeerDirectoryError> {
        envelope.validate()?;
        if envelope.header.scope != self.scope {
            return Err(PeerDirectoryError::ScopeMismatch {
                expected: self.scope.clone(),
                received: envelope.header.scope.clone(),
            });
        }

        let current = self
            .peers
            .get(&voice_client_id)
            .cloned()
            .unwrap_or_default();
        let mut candidate = current;
        let actions = apply_to_peer(voice_client_id, &mut candidate, envelope)?;
        self.peers.insert(voice_client_id, candidate);
        Ok(actions)
    }

    /// Removes one Mumble session, producing the stop/reset work needed by
    /// the ACRE control worker. A duplicate removal is harmless.
    pub fn remove(&mut self, voice_client_id: u32) -> Vec<PeerStateAction> {
        self.peers
            .remove(&voice_client_id)
            .map_or_else(Vec::new, |state| clear_peer(voice_client_id, state, true))
    }

    /// Clears every sender, for example after local channel/server disconnect.
    pub fn clear(&mut self) -> Vec<PeerStateAction> {
        std::mem::take(&mut self.peers)
            .into_iter()
            .flat_map(|(voice_client_id, state)| clear_peer(voice_client_id, state, true))
            .collect()
    }
}

fn apply_to_peer(
    voice_client_id: u32,
    state: &mut PeerState,
    envelope: &PeerEnvelope,
) -> Result<Vec<PeerStateAction>, PeerDirectoryError> {
    let order = state.order.observe(&envelope.header);
    if order == PeerOrderDecision::RejectedStale {
        return Ok(Vec::new());
    }

    let mut actions = if matches!(
        order,
        PeerOrderDecision::AcceptedNewGeneration | PeerOrderDecision::AcceptedNewScope
    ) {
        clear_peer_in_place(voice_client_id, state, true)
    } else {
        Vec::new()
    };

    match &envelope.payload {
        PeerPayload::Hello { acre_version: _ } => {
            // `PeerEnvelope::validate` already pins this to the supported ACRE
            // release. A repeated HELLO within a generation is idempotent.
            state.compatible = true;
        }
        PeerPayload::Identity { net_id } => {
            ensure_handshake(state)?;
            if state.net_id.as_deref() != Some(net_id) {
                actions.extend(clear_active_transmission(state));
                state.net_id = Some(net_id.clone());
                actions.push(PeerStateAction::Identity {
                    voice_client_id,
                    net_id: net_id.clone(),
                });
            }
        }
        PeerPayload::Start {
            kind,
            net_id,
            radio_id,
            language_id,
            curve_scale,
        } => {
            ensure_handshake(state)?;
            if state.net_id.as_deref() != Some(net_id) {
                return Err(PeerDirectoryError::IdentityMismatch);
            }
            let transmission = Transmission::new(
                voice_client_id,
                *language_id,
                net_id.clone(),
                speaking_kind(*kind),
                radio_id.clone(),
                *curve_scale,
            )?;
            if state.active_transmission.as_ref() != Some(&transmission) {
                actions.extend(clear_active_transmission(state));
                state.active_transmission = Some(transmission.clone());
                actions.push(PeerStateAction::TransmissionStarted(transmission));
            }
        }
        PeerPayload::Stop { kind } => {
            ensure_handshake(state)?;
            if state
                .active_transmission
                .as_ref()
                .is_some_and(|active| active.speaking_kind() == speaking_kind(*kind))
            {
                actions.extend(clear_active_transmission(state));
            }
        }
        PeerPayload::Reset => {
            actions.extend(clear_peer_in_place(voice_client_id, state, true));
        }
    }
    Ok(actions)
}

fn ensure_handshake(state: &PeerState) -> Result<(), PeerDirectoryError> {
    state
        .compatible
        .then_some(())
        .ok_or(PeerDirectoryError::HandshakeRequired)
}

fn speaking_kind(kind: PeerSpeakingKind) -> SpeakingKind {
    match kind {
        PeerSpeakingKind::Direct => SpeakingKind::Direct,
        PeerSpeakingKind::Radio => SpeakingKind::Radio,
        PeerSpeakingKind::Intercom => SpeakingKind::Intercom,
        PeerSpeakingKind::God => SpeakingKind::God,
        PeerSpeakingKind::Zeus => SpeakingKind::Zeus,
    }
}

fn clear_active_transmission(state: &mut PeerState) -> Vec<PeerStateAction> {
    state
        .active_transmission
        .take()
        .map_or_else(Vec::new, |transmission| {
            vec![PeerStateAction::TransmissionStopped(transmission)]
        })
}

fn clear_peer_in_place(
    voice_client_id: u32,
    state: &mut PeerState,
    include_reset: bool,
) -> Vec<PeerStateAction> {
    let mut actions = clear_active_transmission(state);
    state.compatible = false;
    state.net_id = None;
    if include_reset {
        actions.push(PeerStateAction::Reset { voice_client_id });
    }
    actions
}

fn clear_peer(
    voice_client_id: u32,
    mut state: PeerState,
    include_reset: bool,
) -> Vec<PeerStateAction> {
    clear_peer_in_place(voice_client_id, &mut state, include_reset)
}

/// Peer-state validation error beyond the bounded wire decoder.
#[derive(Debug, Error)]
pub enum PeerDirectoryError {
    #[error(transparent)]
    Peer(#[from] AcrePeerError),
    #[error(transparent)]
    Session(#[from] AcreSessionError),
    #[error("peer scope {received:?} does not match local scope {expected:?}")]
    ScopeMismatch { expected: String, received: String },
    #[error("peer must send HELLO before identity or transmission state")]
    HandshakeRequired,
    #[error("peer START netId does not match its announced identity")]
    IdentityMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ACRE2_PLUGIN_VERSION;

    fn header(generation: u64, sequence: u64) -> PeerHeader {
        PeerHeader::new("mission-a", generation, sequence).unwrap()
    }

    fn hello(generation: u64, sequence: u64) -> PeerEnvelope {
        PeerEnvelope::new(
            header(generation, sequence),
            PeerPayload::Hello {
                acre_version: ACRE2_PLUGIN_VERSION.to_owned(),
            },
        )
        .unwrap()
    }

    fn identity(generation: u64, sequence: u64, net_id: &str) -> PeerEnvelope {
        PeerEnvelope::new(
            header(generation, sequence),
            PeerPayload::Identity {
                net_id: net_id.to_owned(),
            },
        )
        .unwrap()
    }

    fn radio_start(generation: u64, sequence: u64, net_id: &str) -> PeerEnvelope {
        start(
            PeerSpeakingKind::Radio,
            generation,
            sequence,
            net_id,
            "ACRE_PRC152",
        )
    }

    fn start(
        kind: PeerSpeakingKind,
        generation: u64,
        sequence: u64,
        net_id: &str,
        radio_id: &str,
    ) -> PeerEnvelope {
        PeerEnvelope::new(
            header(generation, sequence),
            PeerPayload::Start {
                kind,
                net_id: net_id.to_owned(),
                radio_id: radio_id.to_owned(),
                language_id: 7,
                curve_scale: 0.75,
            },
        )
        .unwrap()
    }

    #[test]
    fn requires_a_handshake_and_does_not_consume_invalid_sequences() {
        let mut directory = PeerDirectory::new("mission-a").unwrap();
        assert!(matches!(
            directory.apply(99, &identity(1, 1, "2:remote")),
            Err(PeerDirectoryError::HandshakeRequired)
        ));
        assert!(directory.apply(99, &hello(1, 1)).unwrap().is_empty());
        assert_eq!(
            directory.apply(99, &identity(1, 2, "2:remote")).unwrap(),
            vec![PeerStateAction::Identity {
                voice_client_id: 99,
                net_id: "2:remote".to_owned(),
            }]
        );
    }

    #[test]
    fn associates_transmissions_with_the_authoritative_mumble_sender() {
        let mut directory = PeerDirectory::new("mission-a").unwrap();
        directory.apply(99, &hello(1, 1)).unwrap();
        directory.apply(99, &identity(1, 2, "2:remote")).unwrap();

        let started = directory.apply(99, &radio_start(1, 3, "2:remote")).unwrap();
        assert!(matches!(
            &started[..],
            [PeerStateAction::TransmissionStarted(transmission)]
                if transmission.voice_client_id() == 99
                    && transmission.net_id() == "2:remote"
                    && transmission.speaking_kind() == SpeakingKind::Radio
        ));

        assert!(
            directory
                .apply(99, &radio_start(1, 3, "2:remote"))
                .unwrap()
                .is_empty()
        );
        let stopped = directory
            .apply(
                99,
                &PeerEnvelope::new(
                    header(1, 4),
                    PeerPayload::Stop {
                        kind: PeerSpeakingKind::Radio,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            &stopped[..],
            [PeerStateAction::TransmissionStopped(transmission)]
                if transmission.voice_client_id() == 99
        ));
    }

    #[test]
    fn generation_change_stops_the_previous_transmission_before_resetting() {
        let mut directory = PeerDirectory::new("mission-a").unwrap();
        directory.apply(99, &hello(1, 1)).unwrap();
        directory.apply(99, &identity(1, 2, "2:remote")).unwrap();
        directory.apply(99, &radio_start(1, 3, "2:remote")).unwrap();

        let actions = directory.apply(99, &hello(2, 1)).unwrap();
        assert!(matches!(
            actions.as_slice(),
            [
                PeerStateAction::TransmissionStopped(_),
                PeerStateAction::Reset {
                    voice_client_id: 99
                }
            ]
        ));
    }

    #[test]
    fn preserves_god_and_zeus_from_the_authoritative_peer_sender() {
        let mut directory = PeerDirectory::new("mission-a").unwrap();
        directory.apply(99, &hello(1, 1)).unwrap();
        directory.apply(99, &identity(1, 2, "2:remote")).unwrap();

        let god = directory
            .apply(99, &start(PeerSpeakingKind::God, 1, 3, "2:remote", ""))
            .unwrap();
        assert!(matches!(
            god.as_slice(),
            [PeerStateAction::TransmissionStarted(transmission)]
                if transmission.voice_client_id() == 99
                    && transmission.speaking_kind() == SpeakingKind::God
                    && transmission.radio_id().is_empty()
        ));

        let zeus = directory
            .apply(99, &start(PeerSpeakingKind::Zeus, 1, 4, "2:remote", ""))
            .unwrap();
        assert!(matches!(
            zeus.as_slice(),
            [
                PeerStateAction::TransmissionStopped(stopped),
                PeerStateAction::TransmissionStarted(started),
            ] if stopped.voice_client_id() == 99
                && stopped.speaking_kind() == SpeakingKind::God
                && started.voice_client_id() == 99
                && started.speaking_kind() == SpeakingKind::Zeus
                && started.radio_id().is_empty()
        ));
    }

    #[test]
    fn rejects_another_mission_scope_without_creating_peer_state() {
        let mut directory = PeerDirectory::new("mission-a").unwrap();
        let wrong_scope = PeerEnvelope::new(
            PeerHeader::new("mission-b", 1, 1).unwrap(),
            PeerPayload::Hello {
                acre_version: ACRE2_PLUGIN_VERSION.to_owned(),
            },
        )
        .unwrap();
        assert!(matches!(
            directory.apply(99, &wrong_scope),
            Err(PeerDirectoryError::ScopeMismatch { .. })
        ));
        assert!(directory.remove(99).is_empty());
    }
}
