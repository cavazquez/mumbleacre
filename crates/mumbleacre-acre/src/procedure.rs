//! Procedure inventory for ACRE2 `v2.14.0.1064`.
//!
//! These names come from `ACRE2Core/Engine.cpp` plus the replies emitted by its
//! handlers. The inventory is a contract check, not proof that every procedure
//! has already been rendered by the Mumble adapter.

/// Every known wire procedure used by the pinned ACRE2 release.
pub const ACRE2_V214_WIRE_PROCEDURES: &[&str] = &[
    "ext_handleGetClientID",
    "ext_remoteStartSpeaking",
    "ext_remoteStopSpeaking",
    "ext_reset",
    "getClientID",
    "getPluginVersion",
    "getVOIPChannelName",
    "getVOIPChannelUID",
    "getVOIPServerName",
    "getVOIPServerUID",
    "handleGetClientID",
    "handleGetPluginVersion",
    "handleGetVOIPChannelName",
    "handleGetVOIPChannelUID",
    "handleGetVOIPServerName",
    "handleGetVOIPServerUID",
    "handleLoadedSound",
    "handleSoundError",
    "loadSound",
    "localMute",
    "localStartSpeaking",
    "localStopSpeaking",
    "ping",
    "playLoadedSound",
    "pong",
    "remoteStartSpeaking",
    "remoteStopSpeaking",
    "setMuted",
    "setPTTKeys",
    "setSelectableVoiceCurve",
    "setSetting",
    "setSoundSystemMasterOverride",
    "setTs3ChannelDetails",
    "setVoiceCurveModel",
    "startGodModeSpeaking",
    "startIntercomSpeaking",
    "startRadioSpeaking",
    "startZeusSpeaking",
    "stopGodModeSpeaking",
    "stopIntercomSpeaking",
    "stopRadioSpeaking",
    "stopZeusSpeaking",
    "updateSelf",
    "updateSpeakingData",
];

/// Returns whether `procedure` belongs to the pinned ACRE2 wire inventory.
#[must_use]
pub fn is_known_v214_procedure(procedure: &str) -> bool {
    ACRE2_V214_WIRE_PROCEDURES.contains(&procedure)
}

#[cfg(test)]
mod tests {
    use crate::{AcreMessage, is_known_v214_procedure};

    #[test]
    fn every_checked_in_v214_fixture_uses_a_known_procedure() {
        for line in include_str!("../tests/fixtures/acre2-v2.14.0.1064-rpc.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let message = AcreMessage::parse(line.as_bytes()).unwrap();
            assert!(
                is_known_v214_procedure(message.procedure()),
                "fixture procedure {} is missing from the v2.14 inventory",
                message.procedure()
            );
        }
    }
}
