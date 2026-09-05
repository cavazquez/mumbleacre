//! Versioned, transport-independent control messages between Mumble adapters.
//!
//! The protocol contains no secrets: a Mumble plugin receiving `sendData` can
//! inspect its payload. The Mumble callback's sender session remains the
//! authoritative voice identity; this wire format deliberately carries no
//! caller-selected Mumble ID.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ACRE2_PLUGIN_VERSION;

/// Initial peer-control protocol version.
pub const ACRE_PEER_PROTOCOL_VERSION: u8 = 1;

/// Strict ceiling chosen below Mumble's documented `sendData` payload budget.
pub const ACRE_PEER_MAX_MESSAGE_BYTES: usize = 1024;

const MAX_SCOPE_BYTES: usize = 96;
const MAX_NET_ID_BYTES: usize = 128;
const MAX_RADIO_ID_BYTES: usize = 128;
const MAX_ACRE_VERSION_BYTES: usize = 32;

/// Envelope fields that order a message within one Mumble channel/mission
/// scope. `generation` changes when a participant reconnects or changes scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerHeader {
    pub version: u8,
    pub scope: String,
    pub generation: u64,
    pub sequence: u64,
}

impl PeerHeader {
    /// Constructs a header for the pinned initial protocol version.
    pub fn new(
        scope: impl Into<String>,
        generation: u64,
        sequence: u64,
    ) -> Result<Self, AcrePeerError> {
        let header = Self {
            version: ACRE_PEER_PROTOCOL_VERSION,
            scope: scope.into(),
            generation,
            sequence,
        };
        header.validate()?;
        Ok(header)
    }

    /// Validates a programmatically constructed header before it enters
    /// connection state. Envelope validation calls this automatically.
    pub fn validate(&self) -> Result<(), AcrePeerError> {
        if self.version != ACRE_PEER_PROTOCOL_VERSION {
            return Err(AcrePeerError::UnsupportedVersion(self.version));
        }
        validate_token("scope", &self.scope, MAX_SCOPE_BYTES, false)?;
        if self.generation == 0 {
            return Err(AcrePeerError::ZeroGeneration);
        }
        if self.sequence == 0 {
            return Err(AcrePeerError::ZeroSequence);
        }
        Ok(())
    }
}

/// ACRE speaking kind carried by peer `START` and `STOP` messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PeerSpeakingKind {
    Direct,
    Radio,
    Intercom,
    God,
    Zeus,
}

/// The message body. Serialization uses a tagged representation so producers
/// cannot make one variant look like another by changing a field name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum PeerPayload {
    Hello {
        acre_version: String,
    },
    Identity {
        net_id: String,
    },
    Start {
        kind: PeerSpeakingKind,
        net_id: String,
        radio_id: String,
        language_id: i32,
        curve_scale: f32,
    },
    Stop {
        kind: PeerSpeakingKind,
    },
    Reset,
}

/// One complete peer-control message.
///
/// The header and payload are deliberately nested in the JSON representation.
/// Serde cannot safely combine `deny_unknown_fields` with flattened structures;
/// keeping this boundary explicit means unknown fields are rejected before they
/// can influence connection state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerEnvelope {
    pub header: PeerHeader,
    pub payload: PeerPayload,
}

impl PeerEnvelope {
    /// Creates and validates a peer message.
    pub fn new(header: PeerHeader, payload: PeerPayload) -> Result<Self, AcrePeerError> {
        let envelope = Self { header, payload };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Serializes the message as compact JSON for the eventually selected
    /// Mumble control transport.
    pub fn encode(&self) -> Result<Vec<u8>, AcrePeerError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(AcrePeerError::Serialize)?;
        if bytes.len() > ACRE_PEER_MAX_MESSAGE_BYTES {
            return Err(AcrePeerError::MessageTooLong {
                length: bytes.len(),
                limit: ACRE_PEER_MAX_MESSAGE_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decodes a bounded peer message. Callers must bind the result to the
    /// Mumble `sender` callback argument rather than trusting payload identity.
    pub fn decode(bytes: &[u8]) -> Result<Self, AcrePeerError> {
        if bytes.is_empty() {
            return Err(AcrePeerError::EmptyMessage);
        }
        if bytes.len() > ACRE_PEER_MAX_MESSAGE_BYTES {
            return Err(AcrePeerError::MessageTooLong {
                length: bytes.len(),
                limit: ACRE_PEER_MAX_MESSAGE_BYTES,
            });
        }
        let envelope: Self = serde_json::from_slice(bytes).map_err(AcrePeerError::Deserialize)?;
        envelope.validate()?;
        Ok(envelope)
    }

    /// Validates a programmatically constructed envelope before it enters
    /// connection state. `decode` calls this automatically.
    pub fn validate(&self) -> Result<(), AcrePeerError> {
        self.header.validate()?;
        match &self.payload {
            PeerPayload::Hello { acre_version } => {
                validate_token("acre_version", acre_version, MAX_ACRE_VERSION_BYTES, false)?;
                if acre_version != ACRE2_PLUGIN_VERSION {
                    return Err(AcrePeerError::UnsupportedAcreVersion(acre_version.clone()));
                }
            }
            PeerPayload::Identity { net_id } => {
                validate_token("net_id", net_id, MAX_NET_ID_BYTES, false)?;
            }
            PeerPayload::Start {
                kind,
                net_id,
                radio_id,
                curve_scale,
                ..
            } => {
                validate_token("net_id", net_id, MAX_NET_ID_BYTES, false)?;
                validate_token("radio_id", radio_id, MAX_RADIO_ID_BYTES, true)?;
                if !curve_scale.is_finite() {
                    return Err(AcrePeerError::NonFiniteCurveScale);
                }
                match kind {
                    PeerSpeakingKind::Radio if radio_id.is_empty() => {
                        return Err(AcrePeerError::RadioStartWithoutRadioId);
                    }
                    PeerSpeakingKind::Direct
                    | PeerSpeakingKind::Intercom
                    | PeerSpeakingKind::God
                    | PeerSpeakingKind::Zeus
                        if !radio_id.is_empty() =>
                    {
                        return Err(AcrePeerError::UnexpectedRadioId(*kind));
                    }
                    PeerSpeakingKind::Radio
                    | PeerSpeakingKind::Direct
                    | PeerSpeakingKind::Intercom
                    | PeerSpeakingKind::God
                    | PeerSpeakingKind::Zeus => {}
                }
            }
            PeerPayload::Stop { .. } | PeerPayload::Reset => {}
        }
        Ok(())
    }
}

fn validate_token(
    name: &'static str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), AcrePeerError> {
    if value.is_empty() && !allow_empty {
        return Err(AcrePeerError::EmptyField(name));
    }
    if value.len() > max_bytes {
        return Err(AcrePeerError::FieldTooLong {
            field: name,
            length: value.len(),
            limit: max_bytes,
        });
    }
    // Peer values eventually flow into comma-delimited ACRE RPC parameters.
    // Reject separators at the earliest untrusted boundary rather than
    // accepting a peer state that the pipe worker could not serialize.
    if value
        .bytes()
        .any(|byte| !byte.is_ascii_graphic() || byte == b',')
    {
        return Err(AcrePeerError::InvalidField(name));
    }
    Ok(())
}

/// Outcome of checking a header against one remote peer's current stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerOrderDecision {
    AcceptedFirst,
    AcceptedNext,
    AcceptedNewGeneration,
    AcceptedNewScope,
    RejectedStale,
}

/// Per-Mumble-sender sequencing state.
///
/// The caller owns one tracker per authoritative Mumble session ID. A scope or
/// generation transition is accepted but tells the caller to clear old identity
/// and transmit state before applying the payload.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerOrderTracker {
    current: Option<PeerHeader>,
}

impl PeerOrderTracker {
    /// Accepts only strictly newer sequence values in the same scope/generation.
    pub fn observe(&mut self, header: &PeerHeader) -> PeerOrderDecision {
        let decision = match self.current.as_ref() {
            None => PeerOrderDecision::AcceptedFirst,
            Some(current) if current.scope != header.scope => PeerOrderDecision::AcceptedNewScope,
            Some(current) if header.generation > current.generation => {
                PeerOrderDecision::AcceptedNewGeneration
            }
            Some(current)
                if header.generation == current.generation
                    && header.sequence > current.sequence =>
            {
                PeerOrderDecision::AcceptedNext
            }
            Some(_) => PeerOrderDecision::RejectedStale,
        };
        if decision != PeerOrderDecision::RejectedStale {
            self.current = Some(header.clone());
        }
        decision
    }

    /// Clears state on Mumble channel/server disconnect before a new handshake.
    pub fn reset(&mut self) {
        self.current = None;
    }
}

/// Validation or encoding error for the peer-control boundary.
#[derive(Debug, Error)]
pub enum AcrePeerError {
    #[error("peer protocol version {0} is unsupported")]
    UnsupportedVersion(u8),
    #[error("peer ACRE version {0} is unsupported")]
    UnsupportedAcreVersion(String),
    #[error("peer message is empty")]
    EmptyMessage,
    #[error("peer message is {length} bytes; limit is {limit}")]
    MessageTooLong { length: usize, limit: usize },
    #[error("peer field {0} cannot be empty")]
    EmptyField(&'static str),
    #[error("peer field {field} is {length} bytes; limit is {limit}")]
    FieldTooLong {
        field: &'static str,
        length: usize,
        limit: usize,
    },
    #[error("peer field {0} contains unsupported characters")]
    InvalidField(&'static str),
    #[error("peer generation must be non-zero")]
    ZeroGeneration,
    #[error("peer sequence must be non-zero")]
    ZeroSequence,
    #[error("peer START curve scale must be finite")]
    NonFiniteCurveScale,
    #[error("peer radio START requires a radio ID")]
    RadioStartWithoutRadioId,
    #[error("peer {0:?} START must not contain a radio ID")]
    UnexpectedRadioId(PeerSpeakingKind),
    #[error("peer message serialization failed: {0}")]
    Serialize(serde_json::Error),
    #[error("peer message parsing failed: {0}")]
    Deserialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn header(sequence: u64) -> PeerHeader {
        PeerHeader::new("mission-a", 1, sequence).unwrap()
    }

    #[test]
    fn round_trips_a_bounded_radio_start() {
        let envelope = PeerEnvelope::new(
            header(3),
            PeerPayload::Start {
                kind: PeerSpeakingKind::Radio,
                net_id: "2:1234".to_owned(),
                radio_id: "ACRE_PRC152".to_owned(),
                language_id: 7,
                curve_scale: 0.75,
            },
        )
        .unwrap();
        let wire = envelope.encode().unwrap();
        assert!(wire.len() <= ACRE_PEER_MAX_MESSAGE_BYTES);
        assert_eq!(PeerEnvelope::decode(&wire).unwrap(), envelope);
    }

    #[test]
    fn rejects_untrusted_shapes_before_they_enter_peer_state() {
        let bad_radio = PeerEnvelope::new(
            header(1),
            PeerPayload::Start {
                kind: PeerSpeakingKind::Radio,
                net_id: "2:1234".to_owned(),
                radio_id: String::new(),
                language_id: 0,
                curve_scale: 1.0,
            },
        );
        assert!(matches!(
            bad_radio,
            Err(AcrePeerError::RadioStartWithoutRadioId)
        ));

        let malformed = br#"{"header":{"version":1,"scope":"mission-a","generation":1,"sequence":1},"payload":{"type":"IDENTITY","net_id":"2:1","mumble_id":9}}"#;
        assert!(matches!(
            PeerEnvelope::decode(malformed),
            Err(AcrePeerError::Deserialize(_))
        ));

        let comma_in_net_id = PeerEnvelope::new(
            header(2),
            PeerPayload::Identity {
                net_id: "2:remote,other".to_owned(),
            },
        );
        assert!(matches!(
            comma_in_net_id,
            Err(AcrePeerError::InvalidField("net_id"))
        ));

        let oversized = vec![b' '; ACRE_PEER_MAX_MESSAGE_BYTES + 1];
        assert!(matches!(
            PeerEnvelope::decode(&oversized),
            Err(AcrePeerError::MessageTooLong { .. })
        ));
    }

    #[test]
    fn non_radio_peer_starts_reject_radio_ids() {
        for (sequence, kind) in [
            (2, PeerSpeakingKind::Intercom),
            (3, PeerSpeakingKind::God),
            (4, PeerSpeakingKind::Zeus),
        ] {
            assert!(matches!(
                PeerEnvelope::new(
                    header(sequence),
                    PeerPayload::Start {
                        kind,
                        net_id: "2:1234".to_owned(),
                        radio_id: "ACRE_PRC152".to_owned(),
                        language_id: 7,
                        curve_scale: 0.75,
                    },
                ),
                Err(AcrePeerError::UnexpectedRadioId(received)) if received == kind
            ));
        }
    }

    #[test]
    fn order_tracker_requires_strict_progress_and_resets_on_scope_or_generation() {
        let mut tracker = PeerOrderTracker::default();
        assert_eq!(
            tracker.observe(&header(1)),
            PeerOrderDecision::AcceptedFirst
        );
        assert_eq!(
            tracker.observe(&header(1)),
            PeerOrderDecision::RejectedStale
        );
        assert_eq!(tracker.observe(&header(2)), PeerOrderDecision::AcceptedNext);

        let new_generation = PeerHeader::new("mission-a", 2, 1).unwrap();
        assert_eq!(
            tracker.observe(&new_generation),
            PeerOrderDecision::AcceptedNewGeneration
        );
        let new_scope = PeerHeader::new("mission-b", 1, 1).unwrap();
        assert_eq!(
            tracker.observe(&new_scope),
            PeerOrderDecision::AcceptedNewScope
        );
    }

    proptest! {
        #[test]
        fn valid_identity_round_trips(
            scope in "[A-Za-z0-9._:-]{1,32}",
            net_id in "[A-Za-z0-9._:-]{1,64}",
            generation in 1_u64..u64::MAX,
            sequence in 1_u64..u64::MAX,
        ) {
            let envelope = PeerEnvelope::new(
                PeerHeader::new(scope, generation, sequence).unwrap(),
                PeerPayload::Identity { net_id },
            ).unwrap();
            let bytes = envelope.encode().unwrap();
            prop_assert!(bytes.len() <= ACRE_PEER_MAX_MESSAGE_BYTES);
            prop_assert_eq!(PeerEnvelope::decode(&bytes).unwrap(), envelope);
        }

        #[test]
        fn arbitrary_untrusted_bytes_never_panic(
            bytes in prop::collection::vec(any::<u8>(), 0..(ACRE_PEER_MAX_MESSAGE_BYTES + 128)),
        ) {
            let _ = PeerEnvelope::decode(&bytes);
        }
    }
}
