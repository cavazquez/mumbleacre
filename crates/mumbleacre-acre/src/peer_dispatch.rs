//! Delivery of validated peer state to the ACRE pipe owner.
//!
//! Mumble control callbacks authenticate a sender and feed
//! [`crate::PeerDirectory`]. Its actions must then be applied by the one
//! worker that owns an [`crate::AcreSession`] and writes to ACRE's named pipes.
//! This small adapter is transport-independent and deliberately contains no
//! Mumble or pipe I/O.

use crate::{AcreAction, AcreSession, AcreSessionError, PeerStateAction};

/// Applies actions previously accepted by [`crate::PeerDirectory`] to the
/// session that owns the ACRE pipe.
///
/// Identity actions intentionally have no direct ACRE RPC: the validated
/// `netId` travels with each subsequent transmission. A reset defensively
/// closes the sender's active remote transmission even if a caller already
/// supplied its corresponding stop, making duplicate or reordered cleanup
/// harmless.
pub fn apply_peer_state_actions(
    session: &mut AcreSession,
    peer_actions: impl IntoIterator<Item = PeerStateAction>,
) -> Result<Vec<AcreAction>, AcreSessionError> {
    let mut actions = Vec::new();
    for peer_action in peer_actions {
        match peer_action {
            PeerStateAction::Identity { .. } => {}
            PeerStateAction::TransmissionStarted(transmission) => {
                actions.extend(session.remote_started(&transmission)?);
            }
            PeerStateAction::TransmissionStopped(transmission) => {
                actions.extend(session.remote_stopped(transmission.voice_client_id())?);
            }
            PeerStateAction::Reset { voice_client_id } => {
                actions.extend(session.remote_stopped(voice_client_id)?);
            }
        }
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PeerStateAction, SpeakingKind, Transmission};

    fn remote_radio() -> Transmission {
        Transmission::new(91, 4, "2:remote", SpeakingKind::Radio, "ACRE_PRC152", 0.75).unwrap()
    }

    fn remote_privileged(kind: SpeakingKind) -> Transmission {
        Transmission::new(91, 4, "2:remote", kind, "", 0.75).unwrap()
    }

    fn sent_text(actions: &[AcreAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                AcreAction::SendToArma(message) => Some(message.encode_text()),
                AcreAction::LocalTransmissionStarted(_)
                | AcreAction::LocalTransmissionStopped(_)
                | AcreAction::SoundSystemOverrideChanged(_)
                | AcreAction::LocalMuteChanged(_)
                | AcreAction::UserMuteChanged { .. }
                | AcreAction::ListenerUpdated(_)
                | AcreAction::SpeakingUpdated(_)
                | AcreAction::SoundLoaded(_)
                | AcreAction::SoundPlaybackRequested(_)
                | AcreAction::ResetRequested => None,
            })
            .collect()
    }

    #[test]
    fn starts_remote_speech_only_after_the_peer_directory_validates_it() {
        let mut session = AcreSession::default();
        let radio = remote_radio();
        let actions = apply_peer_state_actions(
            &mut session,
            vec![
                PeerStateAction::Identity {
                    voice_client_id: 91,
                    net_id: "2:remote".to_owned(),
                },
                PeerStateAction::TransmissionStarted(radio),
            ],
        )
        .unwrap();

        assert_eq!(
            sent_text(&actions),
            ["remoteStartSpeaking:91,4,2:remote,1,ACRE_PRC152,".to_owned()]
        );
    }

    #[test]
    fn stop_and_reset_emit_at_most_one_remote_stop() {
        let mut session = AcreSession::default();
        let radio = remote_radio();
        apply_peer_state_actions(
            &mut session,
            vec![PeerStateAction::TransmissionStarted(radio.clone())],
        )
        .unwrap();

        let actions = apply_peer_state_actions(
            &mut session,
            vec![
                PeerStateAction::TransmissionStopped(radio),
                PeerStateAction::Reset {
                    voice_client_id: 91,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            sent_text(&actions),
            ["remoteStopSpeaking:91,2:remote,1,ACRE_PRC152,".to_owned()]
        );
    }

    #[test]
    fn reset_without_a_prior_stop_is_fail_closed() {
        let mut session = AcreSession::default();
        apply_peer_state_actions(
            &mut session,
            vec![PeerStateAction::TransmissionStarted(remote_radio())],
        )
        .unwrap();

        let actions = apply_peer_state_actions(
            &mut session,
            vec![PeerStateAction::Reset {
                voice_client_id: 91,
            }],
        )
        .unwrap();

        assert_eq!(
            sent_text(&actions),
            ["remoteStopSpeaking:91,2:remote,1,ACRE_PRC152,".to_owned()]
        );
    }

    #[test]
    fn serializes_remote_god_and_zeus_with_their_acre_wire_values() {
        let mut session = AcreSession::default();
        let god = remote_privileged(SpeakingKind::God);
        let zeus = remote_privileged(SpeakingKind::Zeus);

        let actions = apply_peer_state_actions(
            &mut session,
            vec![
                PeerStateAction::TransmissionStarted(god),
                PeerStateAction::TransmissionStarted(zeus),
            ],
        )
        .unwrap();

        assert_eq!(
            sent_text(&actions),
            [
                "remoteStartSpeaking:91,4,2:remote,5,,".to_owned(),
                "remoteStopSpeaking:91,2:remote,5,,".to_owned(),
                "remoteStartSpeaking:91,4,2:remote,6,,".to_owned(),
            ]
        );
    }
}
