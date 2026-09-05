//! Transport-independent local PTT state for the ACRE2 ↔ Mumble adapter.
//!
//! ACRE owns the radio/intercom/God/Zeus PTT decision while Mumble may also
//! have a native direct-voice PTT or VAD transition. This module serializes
//! those events into idempotent control actions without calling Mumble, writing
//! a pipe, or touching PCM. A later control-plane adapter is responsible for
//! delivering `PeerStarted`/`PeerStopped` through the transport chosen by
//! AM-27.

use thiserror::Error;

use crate::{SpeakingKind, Transmission};

/// One side effect requested by the local PTT state machine.
#[derive(Clone, Debug, PartialEq)]
pub enum PttAction {
    /// Announce a speaking mode to compatible peers through the eventually
    /// selected control transport.
    PeerStarted(Transmission),
    /// End a previously announced speaking mode exactly once.
    PeerStopped(Transmission),
    /// Temporarily force Mumble's microphone open only while ACRE controls a
    /// ACRE-controlled transmission.
    SetMicrophoneActivationOverwrite(bool),
}

/// Serializes local Mumble direct speech with ACRE-controlled PTT.
///
/// At most one mode is announced to peers at a time. A direct Mumble start
/// received while an ACRE PTT is active is remembered as a fallback but is not
/// announced until ACRE stops. This prevents a radio transmission from being
/// classified as direct just because callbacks arrived in a different order.
#[derive(Clone, Debug, Default)]
pub struct AcrePttStateMachine {
    active_acre: Option<Transmission>,
    active_direct: Option<Transmission>,
    suspended_direct: Option<Transmission>,
    microphone_overwrite: bool,
}

impl AcrePttStateMachine {
    /// Processes ACRE's local radio, intercom, God, or Zeus start event.
    ///
    /// A duplicate start is ignored. Changing any ACRE-controlled mode closes
    /// the old one before opening the new one but leaves the microphone
    /// override active throughout the transition.
    pub fn acre_started(
        &mut self,
        transmission: Transmission,
    ) -> Result<Vec<PttAction>, AcrePttError> {
        validate_acre_transmission(&transmission)?;
        if self.active_acre.as_ref() == Some(&transmission) {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        if let Some(previous) = self.active_acre.take() {
            actions.push(PttAction::PeerStopped(previous));
        } else if let Some(direct) = self.active_direct.take() {
            self.suspended_direct = Some(direct.clone());
            actions.push(PttAction::PeerStopped(direct));
        }

        if !self.microphone_overwrite {
            self.microphone_overwrite = true;
            actions.push(PttAction::SetMicrophoneActivationOverwrite(true));
        }
        self.active_acre = Some(transmission.clone());
        actions.push(PttAction::PeerStarted(transmission));
        Ok(actions)
    }

    /// Processes ACRE's local radio, intercom, God, or Zeus stop event.
    ///
    /// A delayed stop for a superseded mode is ignored. If Mumble direct voice
    /// remained active while the ACRE PTT was held, it is restored only after
    /// the override has been released.
    pub fn acre_stopped(&mut self, transmission: &Transmission) -> Vec<PttAction> {
        if self.active_acre.as_ref() != Some(transmission) {
            return Vec::new();
        }
        let stopped = self
            .active_acre
            .take()
            .expect("active ACRE transmission was checked above");
        let mut actions = vec![PttAction::PeerStopped(stopped)];
        if self.microphone_overwrite {
            self.microphone_overwrite = false;
            actions.push(PttAction::SetMicrophoneActivationOverwrite(false));
        }
        if let Some(direct) = self.suspended_direct.take() {
            self.active_direct = Some(direct.clone());
            actions.push(PttAction::PeerStarted(direct));
        }
        actions
    }

    /// Processes Mumble's native direct-voice start event.
    ///
    /// The caller supplies a local `Transmission` populated from trusted local
    /// ACRE identity state. This avoids deriving `netId` from a nickname or
    /// accepting it from a remote peer.
    pub fn direct_started(
        &mut self,
        transmission: Transmission,
    ) -> Result<Vec<PttAction>, AcrePttError> {
        validate_direct_transmission(&transmission)?;
        if self.active_acre.is_some() {
            self.suspended_direct = Some(transmission);
            return Ok(Vec::new());
        }
        if self.active_direct.as_ref() == Some(&transmission) {
            return Ok(Vec::new());
        }

        let mut actions = Vec::new();
        if let Some(previous) = self.active_direct.replace(transmission.clone()) {
            actions.push(PttAction::PeerStopped(previous));
        }
        actions.push(PttAction::PeerStarted(transmission));
        Ok(actions)
    }

    /// Processes Mumble's native direct-voice stop event.
    ///
    /// Passing the original start transmission makes an out-of-order stale
    /// callback harmless after a reconnect or identity refresh.
    pub fn direct_stopped(&mut self, transmission: &Transmission) -> Vec<PttAction> {
        if self.active_acre.is_some() {
            if self.suspended_direct.as_ref() == Some(transmission) {
                self.suspended_direct = None;
            }
            return Vec::new();
        }
        if self.active_direct.as_ref() != Some(transmission) {
            return Vec::new();
        }
        let stopped = self
            .active_direct
            .take()
            .expect("active direct transmission was checked above");
        vec![PttAction::PeerStopped(stopped)]
    }

    /// Stops the current native voice from a fresh host audio observation.
    /// Unlike an event carrying old metadata, this observes the current input,
    /// so language/curve changes cannot leave the previous direct TX active.
    pub fn native_stopped(&mut self) -> Vec<PttAction> {
        let transmission = self
            .suspended_direct
            .as_ref()
            .or(self.active_direct.as_ref())
            .cloned();
        transmission.map_or_else(Vec::new, |t| self.direct_stopped(&t))
    }

    /// Clears every locally announced mode on disconnect, reset, or shutdown.
    ///
    /// The returned order always stops peer state before releasing Mumble's
    /// override, so a failure cannot leave an ACRE transmission pinned open.
    pub fn reset(&mut self) -> Vec<PttAction> {
        let mut actions = Vec::new();
        if let Some(transmission) = self.active_acre.take() {
            actions.push(PttAction::PeerStopped(transmission));
        }
        if let Some(transmission) = self.active_direct.take() {
            actions.push(PttAction::PeerStopped(transmission));
        }
        self.suspended_direct = None;
        if self.microphone_overwrite {
            self.microphone_overwrite = false;
            actions.push(PttAction::SetMicrophoneActivationOverwrite(false));
        }
        actions
    }

    #[must_use]
    pub fn active_acre(&self) -> Option<&Transmission> {
        self.active_acre.as_ref()
    }

    #[must_use]
    pub const fn microphone_overwrite_active(&self) -> bool {
        self.microphone_overwrite
    }
}

fn validate_acre_transmission(transmission: &Transmission) -> Result<(), AcrePttError> {
    match transmission.speaking_kind() {
        SpeakingKind::Radio if transmission.radio_id().is_empty() => {
            Err(AcrePttError::RadioWithoutRadioId)
        }
        SpeakingKind::Intercom | SpeakingKind::God | SpeakingKind::Zeus
            if !transmission.radio_id().is_empty() =>
        {
            Err(AcrePttError::NonRadioWithRadioId(
                transmission.speaking_kind(),
            ))
        }
        SpeakingKind::Radio | SpeakingKind::Intercom | SpeakingKind::God | SpeakingKind::Zeus => {
            Ok(())
        }
        other => Err(AcrePttError::UnsupportedAcreSpeakingKind(other)),
    }
}

fn validate_direct_transmission(transmission: &Transmission) -> Result<(), AcrePttError> {
    if transmission.speaking_kind() != SpeakingKind::Direct {
        return Err(AcrePttError::ExpectedDirect(transmission.speaking_kind()));
    }
    if !transmission.radio_id().is_empty() {
        return Err(AcrePttError::DirectWithRadioId);
    }
    Ok(())
}

/// Invalid local control input rejected before it can affect peer state.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AcrePttError {
    #[error("ACRE PTT only supports local radio, intercom, God, or Zeus, not {0:?}")]
    UnsupportedAcreSpeakingKind(SpeakingKind),
    #[error("a local radio PTT requires a radio ID")]
    RadioWithoutRadioId,
    #[error("ACRE local {0:?} PTT must not carry a radio ID")]
    NonRadioWithRadioId(SpeakingKind),
    #[error("expected local direct speech, received {0:?}")]
    ExpectedDirect(SpeakingKind),
    #[error("direct Mumble speech must not carry a radio ID")]
    DirectWithRadioId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transmission(kind: SpeakingKind, radio_id: &str) -> Transmission {
        Transmission::new(17, 4, "2:local", kind, radio_id, 0.75).unwrap()
    }

    #[test]
    fn radio_start_stops_direct_then_restores_it_after_ptt_release() {
        let direct = transmission(SpeakingKind::Direct, "");
        let radio = transmission(SpeakingKind::Radio, "ACRE_PRC152");
        let mut machine = AcrePttStateMachine::default();

        assert_eq!(
            machine.direct_started(direct.clone()).unwrap(),
            vec![PttAction::PeerStarted(direct.clone())]
        );
        assert_eq!(
            machine.acre_started(radio.clone()).unwrap(),
            vec![
                PttAction::PeerStopped(direct.clone()),
                PttAction::SetMicrophoneActivationOverwrite(true),
                PttAction::PeerStarted(radio.clone()),
            ]
        );
        assert!(machine.microphone_overwrite_active());
        assert_eq!(
            machine.acre_stopped(&radio),
            vec![
                PttAction::PeerStopped(radio),
                PttAction::SetMicrophoneActivationOverwrite(false),
                PttAction::PeerStarted(direct),
            ]
        );
        assert!(!machine.microphone_overwrite_active());
    }

    #[test]
    fn duplicate_and_stale_ptt_callbacks_are_idempotent() {
        let radio = transmission(SpeakingKind::Radio, "ACRE_PRC152");
        let intercom = transmission(SpeakingKind::Intercom, "");
        let mut machine = AcrePttStateMachine::default();

        let first = machine.acre_started(radio.clone()).unwrap();
        assert_eq!(first.len(), 2);
        assert!(machine.acre_started(radio.clone()).unwrap().is_empty());
        assert!(machine.acre_stopped(&intercom).is_empty());
        assert_eq!(machine.acre_stopped(&radio).len(), 2);
        assert!(machine.acre_stopped(&radio).is_empty());
    }

    #[test]
    fn switching_acre_modes_keeps_microphone_override_stable() {
        let radio = transmission(SpeakingKind::Radio, "ACRE_PRC152");
        let intercom = transmission(SpeakingKind::Intercom, "");
        let mut machine = AcrePttStateMachine::default();

        machine.acre_started(radio.clone()).unwrap();
        assert_eq!(
            machine.acre_started(intercom.clone()).unwrap(),
            vec![
                PttAction::PeerStopped(radio),
                PttAction::PeerStarted(intercom.clone()),
            ]
        );
        assert!(machine.microphone_overwrite_active());
        assert_eq!(
            machine.acre_stopped(&intercom),
            vec![
                PttAction::PeerStopped(intercom),
                PttAction::SetMicrophoneActivationOverwrite(false),
            ]
        );
    }

    #[test]
    fn god_and_zeus_share_the_acre_ptt_transition_without_releasing_override() {
        let god = transmission(SpeakingKind::God, "");
        let zeus = transmission(SpeakingKind::Zeus, "");
        let mut machine = AcrePttStateMachine::default();

        assert_eq!(
            machine.acre_started(god.clone()).unwrap(),
            vec![
                PttAction::SetMicrophoneActivationOverwrite(true),
                PttAction::PeerStarted(god.clone()),
            ]
        );
        assert_eq!(
            machine.acre_started(zeus.clone()).unwrap(),
            vec![
                PttAction::PeerStopped(god),
                PttAction::PeerStarted(zeus.clone()),
            ]
        );
        assert!(machine.microphone_overwrite_active());
        assert_eq!(
            machine.acre_stopped(&zeus),
            vec![
                PttAction::PeerStopped(zeus),
                PttAction::SetMicrophoneActivationOverwrite(false),
            ]
        );
        assert!(!machine.microphone_overwrite_active());
    }

    #[test]
    fn direct_callbacks_during_radio_are_suspended_and_stale_stops_are_harmless() {
        let radio = transmission(SpeakingKind::Radio, "ACRE_PRC152");
        let direct = transmission(SpeakingKind::Direct, "");
        let stale = Transmission::new(17, 4, "2:other", SpeakingKind::Direct, "", 0.75).unwrap();
        let mut machine = AcrePttStateMachine::default();

        machine.acre_started(radio.clone()).unwrap();
        assert!(machine.direct_started(direct.clone()).unwrap().is_empty());
        assert!(machine.direct_stopped(&stale).is_empty());
        assert!(machine.direct_stopped(&direct).is_empty());
        assert_eq!(
            machine.acre_stopped(&radio),
            vec![
                PttAction::PeerStopped(radio),
                PttAction::SetMicrophoneActivationOverwrite(false),
            ]
        );
    }

    #[test]
    fn reset_stops_once_and_releases_override() {
        let radio = transmission(SpeakingKind::Radio, "ACRE_PRC152");
        let mut machine = AcrePttStateMachine::default();
        machine.acre_started(radio.clone()).unwrap();

        assert_eq!(
            machine.reset(),
            vec![
                PttAction::PeerStopped(radio),
                PttAction::SetMicrophoneActivationOverwrite(false),
            ]
        );
        assert!(machine.reset().is_empty());
    }

    #[test]
    fn invalid_modes_do_not_mutate_the_machine() {
        let mut machine = AcrePttStateMachine::default();
        let direct = transmission(SpeakingKind::Direct, "");
        assert_eq!(
            machine.acre_started(direct),
            Err(AcrePttError::UnsupportedAcreSpeakingKind(
                SpeakingKind::Direct
            ))
        );
        assert!(machine.active_acre().is_none());
        assert!(!machine.microphone_overwrite_active());
    }

    #[test]
    fn non_radio_acre_modes_reject_radio_ids() {
        for kind in [
            SpeakingKind::Intercom,
            SpeakingKind::God,
            SpeakingKind::Zeus,
        ] {
            let mut machine = AcrePttStateMachine::default();
            assert_eq!(
                machine.acre_started(transmission(kind, "ACRE_PRC152")),
                Err(AcrePttError::NonRadioWithRadioId(kind))
            );
            assert!(machine.active_acre().is_none());
            assert!(!machine.microphone_overwrite_active());
        }
    }
}
