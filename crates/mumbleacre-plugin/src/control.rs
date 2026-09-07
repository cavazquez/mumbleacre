//! Full-state peer updates recover from a lost start, stop, or initial handshake.
use mumbleacre_acre::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const DATA_ID: &std::ffi::CStr = c"MumbleACRE:STATE:1";
pub const LEASE: Duration = Duration::from_millis(250);
pub const REFRESH: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMessage {
    pub header: PeerHeader,
    pub acre_version: String,
    pub net_id: String,
    pub active: Option<PeerPayload>,
}
impl StateMessage {
    pub fn new(
        scope: &str,
        generation: u64,
        sequence: u64,
        net_id: &str,
        tx: Option<&Transmission>,
    ) -> Result<Self, String> {
        let active = tx.map(|t| PeerPayload::Start {
            kind: match t.speaking_kind() {
                SpeakingKind::Direct => PeerSpeakingKind::Direct,
                SpeakingKind::Radio => PeerSpeakingKind::Radio,
                SpeakingKind::Intercom => PeerSpeakingKind::Intercom,
                SpeakingKind::God => PeerSpeakingKind::God,
                _ => PeerSpeakingKind::Zeus,
            },
            net_id: t.net_id().to_owned(),
            radio_id: t.radio_id().to_owned(),
            language_id: t.language_id(),
            curve_scale: t.curve_scale(),
        });
        let state = Self {
            header: PeerHeader::new(scope, generation, sequence).map_err(|e| e.to_string())?,
            acre_version: ACRE2_PLUGIN_VERSION.into(),
            net_id: net_id.into(),
            active,
        };
        state.validate()?;
        Ok(state)
    }
    fn validate(&self) -> Result<(), String> {
        for payload in [
            PeerPayload::Hello {
                acre_version: self.acre_version.clone(),
            },
            PeerPayload::Identity {
                net_id: self.net_id.clone(),
            },
        ] {
            PeerEnvelope::new(self.header.clone(), payload).map_err(|e| e.to_string())?;
        }
        if let Some(payload) = &self.active {
            match payload {
                PeerPayload::Start { net_id, kind, .. } if net_id == &self.net_id => {
                    // Privileged starts require additional game-side authorization.
                    // Until that exists, never let a peer claim God/Zeus remotely.
                    if matches!(kind, PeerSpeakingKind::God | PeerSpeakingKind::Zeus) {
                        return Err("privileged peer mode unsupported".into());
                    }
                }
                _ => return Err("state requires a matching START".into()),
            }
            PeerEnvelope::new(self.header.clone(), payload.clone()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if bytes.len() > ACRE_PEER_MAX_MESSAGE_BYTES {
            return Err("state exceeds 1024 bytes".into());
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > ACRE_PEER_MAX_MESSAGE_BYTES {
            return Err("invalid state length".into());
        }
        let state: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        state.validate()?;
        Ok(state)
    }
    fn transmission(&self, sender: u32) -> Result<Option<Transmission>, String> {
        let Some(PeerPayload::Start {
            kind,
            net_id,
            radio_id,
            language_id,
            curve_scale,
        }) = &self.active
        else {
            return Ok(None);
        };
        let kind = match kind {
            PeerSpeakingKind::Direct => SpeakingKind::Direct,
            PeerSpeakingKind::Radio => SpeakingKind::Radio,
            PeerSpeakingKind::Intercom => SpeakingKind::Intercom,
            _ => return Err("unsupported mode".into()),
        };
        Transmission::new(sender, *language_id, net_id, kind, radio_id, *curve_scale)
            .map(Some)
            .map_err(|e| e.to_string())
    }
}
struct RemoteState {
    header: PeerHeader,
    net_id: String,
    active: Option<Transmission>,
    refreshed: Instant,
}
#[derive(Default)]
pub struct RemotePeers {
    states: HashMap<u32, RemoteState>,
}
impl RemotePeers {
    pub fn receive(
        &mut self,
        sender: u32,
        scope: &str,
        state: StateMessage,
        now: Instant,
    ) -> Result<Vec<PeerStateAction>, String> {
        state.validate()?;
        if state.header.scope != scope {
            return Err("mission scope mismatch".into());
        }
        if let Some(old) = self.states.get(&sender) {
            if state.header.generation < old.header.generation
                || (state.header.generation == old.header.generation
                    && state.header.sequence <= old.header.sequence)
            {
                return Ok(vec![]);
            }
        } else if self.states.len() >= ACRE_AUDIO_MAX_SPEAKERS {
            return Err("peer capacity reached".into());
        }
        let active = state.transmission(sender)?;
        let old = self.states.get(&sender);
        let replaced = old.is_some_and(|old| {
            old.header.generation != state.header.generation || old.net_id != state.net_id
        });
        let changed = replaced || old.and_then(|o| o.active.as_ref()) != active.as_ref();
        let mut actions = vec![];
        if changed && let Some(tx) = old.and_then(|o| o.active.clone()) {
            actions.push(PeerStateAction::TransmissionStopped(tx));
        }
        if old.is_none() || replaced {
            actions.push(PeerStateAction::Identity {
                voice_client_id: sender,
                net_id: state.net_id.clone(),
            });
        }
        if changed && let Some(tx) = active.clone() {
            actions.push(PeerStateAction::TransmissionStarted(tx));
        }
        self.states.insert(
            sender,
            RemoteState {
                header: state.header,
                net_id: state.net_id,
                active,
                refreshed: now,
            },
        );
        Ok(actions)
    }
    pub fn expire(&mut self, now: Instant) -> Vec<PeerStateAction> {
        let mut actions = vec![];
        for state in self.states.values_mut() {
            if now.saturating_duration_since(state.refreshed) >= LEASE
                && let Some(tx) = state.active.take()
            {
                actions.push(PeerStateAction::TransmissionStopped(tx));
            }
        }
        actions
    }
    pub fn remove(&mut self, sender: u32) -> Vec<PeerStateAction> {
        let Some(state) = self.states.remove(&sender) else {
            return vec![];
        };
        let mut actions = vec![];
        if let Some(tx) = state.active {
            actions.push(PeerStateAction::TransmissionStopped(tx));
        }
        actions.push(PeerStateAction::Reset {
            voice_client_id: sender,
        });
        actions
    }
    pub fn ids(&self) -> Vec<u32> {
        self.states.keys().copied().collect()
    }
    pub fn audible(&self, now: Instant) -> HashMap<u32, Instant> {
        self.states
            .iter()
            .filter(|(_, s)| {
                s.active.is_some() && now.saturating_duration_since(s.refreshed) < LEASE
            })
            .map(|(id, s)| (*id, s.refreshed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SimulatedMumbleClient {
        voice_id: u32,
        net_id: String,
        generation: u64,
        sequence: u64,
        remote_peers: RemotePeers,
        acre: AcreSession,
    }

    impl SimulatedMumbleClient {
        fn new(voice_id: u32, net_id: &str) -> Self {
            Self {
                voice_id,
                net_id: net_id.to_owned(),
                generation: 1,
                sequence: 0,
                remote_peers: RemotePeers::default(),
                acre: AcreSession::default(),
            }
        }

        fn publish(&mut self, active: Option<&Transmission>) -> Vec<u8> {
            self.sequence += 1;
            StateMessage::new(
                "mumble-channel",
                self.generation,
                self.sequence,
                &self.net_id,
                active,
            )
            .unwrap()
            .encode()
            .unwrap()
        }

        fn reconnect(&mut self) {
            self.generation += 1;
            self.sequence = 0;
        }

        fn receive(&mut self, sender: u32, packet: &[u8], now: Instant) -> Vec<String> {
            let state = StateMessage::decode(packet).unwrap();
            let actions = self
                .remote_peers
                .receive(sender, "mumble-channel", state, now)
                .unwrap();
            sent_text(&apply_peer_state_actions(&mut self.acre, actions).unwrap())
        }

        fn expire(&mut self, now: Instant) -> Vec<String> {
            sent_text(
                &apply_peer_state_actions(&mut self.acre, self.remote_peers.expire(now)).unwrap(),
            )
        }
    }

    fn sent_text(actions: &[AcreAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                AcreAction::SendToArma(message) => Some(message.encode_text()),
                _ => None,
            })
            .collect()
    }

    fn state(sequence: u64, active: bool) -> StateMessage {
        let tx =
            Transmission::new(999, 0, "1:2", SpeakingKind::Radio, "ACRE_PRC152_ID_1", 1.0).unwrap();
        StateMessage::new("mission", 1, sequence, "1:2", active.then_some(&tx)).unwrap()
    }
    #[test]
    fn full_state_recovers_lost_handshake_start_stop_and_uses_sender() {
        let mut peers = RemotePeers::default();
        let now = Instant::now();
        let actions = peers
            .receive(
                42,
                "mission",
                StateMessage::decode(&state(3, true).encode().unwrap()).unwrap(),
                now,
            )
            .unwrap();
        assert!(
            matches!(&actions[1],PeerStateAction::TransmissionStarted(t) if t.voice_client_id()==42)
        );
        assert!(
            peers
                .receive(42, "mission", state(4, true), now)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            &peers.expire(now + LEASE)[0],
            PeerStateAction::TransmissionStopped(_)
        ));
        assert!(peers.expire(now + LEASE).is_empty());
        assert!(
            peers
                .receive(42, "mission", state(3, true), now + LEASE)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            &peers
                .receive(42, "mission", state(6, true), now + LEASE)
                .unwrap()[0],
            PeerStateAction::TransmissionStarted(_)
        ));
        assert!(matches!(
            &peers
                .receive(42, "mission", state(8, false), now + LEASE)
                .unwrap()[0],
            PeerStateAction::TransmissionStopped(_)
        ));
    }
    #[test]
    fn wrong_scope_and_bad_payload_do_not_consume_sequence() {
        let mut peers = RemotePeers::default();
        let now = Instant::now();
        assert!(peers.receive(42, "other", state(1, true), now).is_err());
        let mut bad = state(1, true);
        bad.active = Some(PeerPayload::Reset);
        assert!(peers.receive(42, "mission", bad, now).is_err());
        assert_eq!(
            peers
                .receive(42, "mission", state(1, true), now)
                .unwrap()
                .len(),
            2
        );
        assert!(StateMessage::decode(&vec![0; 1025]).is_err());
    }
    #[test]
    fn two_sessions_deliver_radio_start_and_expired_stop_to_acre() {
        let mut sender = AcreSession::default();
        sender.set_local_voice_client_id(Some(42));
        sender
            .handle_from_arma(
                &AcreMessage::parse(b"getClientID:1:2,76561198000000000,").unwrap(),
                0.0,
            )
            .unwrap();
        let actions = sender
            .handle_from_arma(
                &AcreMessage::parse(b"startRadioSpeaking:ACRE_PRC152_ID_1,").unwrap(),
                0.0,
            )
            .unwrap();
        let tx = actions
            .iter()
            .find_map(|action| match action {
                AcreAction::LocalTransmissionStarted(t) => Some(t),
                _ => None,
            })
            .unwrap();
        let packet = StateMessage::new("mission", 1, 1, "1:2", Some(tx))
            .unwrap()
            .encode()
            .unwrap();
        let now = Instant::now();
        let mut peers = RemotePeers::default();
        let mut receiver = AcreSession::default();
        let peer_actions = peers
            .receive(42, "mission", StateMessage::decode(&packet).unwrap(), now)
            .unwrap();
        let rpc = apply_peer_state_actions(&mut receiver, peer_actions).unwrap();
        assert!(
            matches!(&rpc[0], AcreAction::SendToArma(m) if m.encode_text() == "remoteStartSpeaking:42,0,1:2,1,ACRE_PRC152_ID_1,")
        );
        let rpc = apply_peer_state_actions(&mut receiver, peers.expire(now + LEASE)).unwrap();
        assert!(
            matches!(&rpc[0], AcreAction::SendToArma(m) if m.encode_text() == "remoteStopSpeaking:42,1:2,1,ACRE_PRC152_ID_1,")
        );
        assert!(peers.audible(now + LEASE).is_empty());
    }

    #[test]
    fn newer_generation_closes_old_transmission_and_rejects_late_state() {
        let now = Instant::now();
        let mut peers = RemotePeers::default();
        peers.receive(42, "mission", state(8, true), now).unwrap();
        let mut replacement = state(1, true);
        replacement.header.generation = 2;
        let actions = peers.receive(42, "mission", replacement, now).unwrap();
        assert!(matches!(
            &actions[0],
            PeerStateAction::TransmissionStopped(_)
        ));
        assert!(matches!(
            &actions[2],
            PeerStateAction::TransmissionStarted(_)
        ));
        assert!(
            peers
                .receive(42, "mission", state(999, false), now)
                .unwrap()
                .is_empty()
        );
        assert_eq!(peers.audible(now).len(), 1);
    }

    #[test]
    fn two_simulated_mumble_clients_recover_from_lost_stop_and_reconnect() {
        let now = Instant::now();
        let mut alice = SimulatedMumbleClient::new(42, "1:alice");
        let mut bob = SimulatedMumbleClient::new(77, "1:bob");
        let alice_radio = Transmission::new(
            alice.voice_id,
            0,
            &alice.net_id,
            SpeakingKind::Radio,
            "ACRE_PRC152_ID_1",
            1.0,
        )
        .unwrap();
        let bob_direct =
            Transmission::new(bob.voice_id, 0, &bob.net_id, SpeakingKind::Direct, "", 1.0).unwrap();

        let alice_packet = alice.publish(Some(&alice_radio));
        assert_eq!(
            bob.receive(alice.voice_id, &alice_packet, now),
            ["remoteStartSpeaking:42,0,1:alice,1,ACRE_PRC152_ID_1,".to_owned()]
        );
        let bob_packet = bob.publish(Some(&bob_direct));
        assert_eq!(
            alice.receive(bob.voice_id, &bob_packet, now),
            ["remoteStartSpeaking:77,0,1:bob,0,,".to_owned()]
        );

        // Alice's STOP is lost. A pipe reconnect starts a new generation with
        // a complete state, so Bob first closes the old RPC then opens the new
        // transmission without relying on the missing packet.
        let delayed_old_stop = alice.publish(None);
        alice.reconnect();
        let reconnect_packet = alice.publish(Some(&alice_radio));
        assert_eq!(
            bob.receive(
                alice.voice_id,
                &reconnect_packet,
                now + Duration::from_millis(10)
            ),
            [
                "remoteStopSpeaking:42,1:alice,1,ACRE_PRC152_ID_1,".to_owned(),
                "remoteStartSpeaking:42,0,1:alice,1,ACRE_PRC152_ID_1,".to_owned(),
            ]
        );
        assert!(
            bob.receive(
                alice.voice_id,
                &delayed_old_stop,
                now + Duration::from_millis(20)
            )
            .is_empty()
        );

        assert_eq!(
            bob.expire(now + LEASE + Duration::from_millis(10)),
            ["remoteStopSpeaking:42,1:alice,1,ACRE_PRC152_ID_1,".to_owned()]
        );
        assert_eq!(
            alice.expire(now + LEASE + Duration::from_millis(10)),
            ["remoteStopSpeaking:77,1:bob,0,,".to_owned()]
        );
    }
}
