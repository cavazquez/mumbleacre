//! Typed, bounded ACRE2 loaded-sound control messages.
//!
//! ACRE2 v2.14 transfers a base64 WAV as a sequence of `loadSound` RPCs and
//! then asks the voice backend to play it with `playLoadedSound`.  The stock
//! core appends chunks without validating their counters; this module instead
//! makes the transfer explicit, bounded, and suitable for a non-RT worker.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{AcreMessage, AcreSpeakerVector};

/// Maximum independent sound transfers held before a completed sound reaches
/// the preparation worker.
pub const ACRE_MAX_PENDING_SOUNDS: usize = 32;
/// ACRE's checked-in v2.14 source uses 2 KiB chunks. This ceiling permits a
/// future modest increase while keeping one wire message comfortably bounded.
pub const ACRE_MAX_SOUND_CHUNK_BYTES: usize = 3_072;
/// Upper bound for a single base64 payload before decoding.
pub const ACRE_MAX_SOUND_ENCODED_BYTES: usize = 256 * 1_024;
/// Upper bound for the decoded WAV bytes of one ACRE sound.
pub const ACRE_MAX_SOUND_DECODED_BYTES: usize = (ACRE_MAX_SOUND_ENCODED_BYTES / 4) * 3;
/// Upper bound for the number of chunks declared by one transfer.
pub const ACRE_MAX_SOUND_CHUNKS: usize = 128;
/// Incomplete transfers cannot remain in the control plane indefinitely.
pub const ACRE_SOUND_LOAD_TIMEOUT: Duration = Duration::from_secs(10);

const ACRE_MAX_SOUND_ID_BYTES: usize = 128;
const ACRE_MAX_PENDING_SOUND_ENCODED_BYTES: usize =
    ACRE_MAX_PENDING_SOUNDS * ACRE_MAX_SOUND_ENCODED_BYTES;

/// One validated `loadSound` chunk. `index` preserves ACRE's wire counter;
/// the assembler recognizes both the current one-based producer and the old
/// zero-based fixture form to make a release upgrade explicit rather than
/// silently reordering data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcreSoundLoadChunk {
    id: String,
    index: usize,
    total: usize,
    base64: String,
}

impl AcreSoundLoadChunk {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    #[must_use]
    pub fn base64(&self) -> &str {
        &self.base64
    }
}

/// A completely assembled and decoded ACRE sound. Its SHA-256 identifies the
/// exact bytes independently of its mission-controlled ID; it is an integrity
/// key for the local cache, not an authentication claim about the private pipe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcreLoadedSound {
    id: String,
    bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl AcreLoadedSound {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Validated `playLoadedSound` request. Coordinates retain the native ACRE
/// mixer order (`x`, `z`, `y`) so a later Mumble renderer does not reinterpret
/// world space while scheduling a prepared local sample.
#[derive(Clone, Debug, PartialEq)]
pub struct AcreSoundPlayback {
    id: String,
    position: AcreSpeakerVector,
    direction: AcreSpeakerVector,
    volume: f32,
    is_world: bool,
}

impl AcreSoundPlayback {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn position(&self) -> AcreSpeakerVector {
        self.position
    }

    #[must_use]
    pub const fn direction(&self) -> AcreSpeakerVector {
        self.direction
    }

    #[must_use]
    pub const fn volume(&self) -> f32 {
        self.volume
    }

    #[must_use]
    pub const fn is_world(&self) -> bool {
        self.is_world
    }
}

/// Parses a single `loadSound:id,index,total,base64` message from ACRE2.
pub fn parse_load_sound(message: &AcreMessage) -> Result<AcreSoundLoadChunk, AcreSoundError> {
    expect_procedure(message, "loadSound")?;
    expect_arity(message, 4)?;
    let id = validated_sound_id(&message.parameters()[0])?;
    let index = parse_usize(message, 1)?;
    let total = parse_usize(message, 2)?;
    if total == 0 || total > ACRE_MAX_SOUND_CHUNKS {
        return Err(AcreSoundError::InvalidChunkTotal { total });
    }
    // The live v2.14 sender counts 1..=total. Keep the repository's older
    // 0..total-1 fixture readable so a fixture update cannot hide a wire
    // compatibility regression. The assembler fixes the basis per transfer.
    if index > total || (index == total && index == 0) {
        return Err(AcreSoundError::InvalidChunkIndex { index, total });
    }
    let base64 = message.parameters()[3].clone();
    if base64.len() > ACRE_MAX_SOUND_CHUNK_BYTES {
        return Err(AcreSoundError::ChunkTooLong {
            length: base64.len(),
            limit: ACRE_MAX_SOUND_CHUNK_BYTES,
        });
    }
    if !base64
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err(AcreSoundError::InvalidBase64);
    }

    Ok(AcreSoundLoadChunk {
        id,
        index,
        total,
        base64,
    })
}

/// Parses a `playLoadedSound` request using the exact v2.14 parameter layout.
pub fn parse_play_loaded_sound(message: &AcreMessage) -> Result<AcreSoundPlayback, AcreSoundError> {
    expect_procedure(message, "playLoadedSound")?;
    expect_arity(message, 9)?;
    let id = validated_sound_id(&message.parameters()[0])?;
    let position = parse_vector(message, 1)?;
    let direction = parse_vector(message, 4)?;
    let volume = parse_finite_f32(message, 7)?;
    if !(0.0..=1.0).contains(&volume) {
        return Err(AcreSoundError::InvalidVolume { volume });
    }
    let is_world = match message.parameters()[8].as_str() {
        "0" => false,
        "1" => true,
        value => return Err(AcreSoundError::InvalidWorldFlag(value.to_owned())),
    };

    Ok(AcreSoundPlayback {
        id,
        position,
        direction,
        volume,
        is_world,
    })
}

/// Bounded state for in-progress ACRE sound transfers. It owns only encoded
/// chunks; WAV parsing and file preparation deliberately happen in a later
/// worker after [`Self::accept`] yields an [`AcreLoadedSound`].
#[derive(Debug, Default)]
pub struct AcreSoundAssembler {
    pending: HashMap<String, PendingSound>,
    pending_encoded_bytes: usize,
}

#[derive(Debug)]
struct PendingSound {
    total: usize,
    zero_based: bool,
    chunks: Vec<Option<String>>,
    encoded_bytes: usize,
    last_update: Instant,
}

impl AcreSoundAssembler {
    /// Accepts one chunk and yields decoded bytes only after every declared
    /// part arrived. Repeated identical chunks are idempotent; conflicting
    /// chunks discard that ID's pending transfer rather than combining data.
    pub fn accept(
        &mut self,
        chunk: AcreSoundLoadChunk,
        now: Instant,
    ) -> Result<Option<AcreLoadedSound>, AcreSoundError> {
        let _ = self.expire(now);
        let id = chunk.id.clone();
        let zero_based = chunk.index == 0;
        let slot = chunk_slot(chunk.index, chunk.total, zero_based)?;
        let starts_new = !self.pending.contains_key(&id);
        if starts_new {
            if self.pending.len() >= ACRE_MAX_PENDING_SOUNDS {
                return Err(AcreSoundError::TooManyPendingSounds {
                    limit: ACRE_MAX_PENDING_SOUNDS,
                });
            }
            self.pending.insert(
                id.clone(),
                PendingSound {
                    total: chunk.total,
                    zero_based,
                    chunks: vec![None; chunk.total],
                    encoded_bytes: 0,
                    last_update: now,
                },
            );
        }

        let mut failure = None;
        let mut complete = false;
        {
            let pending = self
                .pending
                .get_mut(&id)
                .expect("new ACRE sound transfer is inserted before access");
            if pending.total != chunk.total || pending.zero_based != zero_based {
                failure = Some(AcreSoundError::ConflictingChunkLayout { id: id.clone() });
            } else if slot >= pending.chunks.len() {
                failure = Some(AcreSoundError::InvalidChunkIndex {
                    index: chunk.index,
                    total: chunk.total,
                });
            } else if chunk.base64.is_empty() && slot + 1 != pending.total {
                failure = Some(AcreSoundError::EmptyNonFinalChunk { id: id.clone() });
            } else if let Some(existing) = &pending.chunks[slot] {
                if existing == &chunk.base64 {
                    pending.last_update = now;
                } else {
                    failure = Some(AcreSoundError::ConflictingChunk { id: id.clone() });
                }
            } else {
                let next_item_bytes = pending.encoded_bytes.saturating_add(chunk.base64.len());
                let next_total_bytes = self
                    .pending_encoded_bytes
                    .saturating_add(chunk.base64.len());
                if next_item_bytes > ACRE_MAX_SOUND_ENCODED_BYTES {
                    failure = Some(AcreSoundError::EncodedSoundTooLarge {
                        id: id.clone(),
                        length: next_item_bytes,
                        limit: ACRE_MAX_SOUND_ENCODED_BYTES,
                    });
                } else if next_total_bytes > ACRE_MAX_PENDING_SOUND_ENCODED_BYTES {
                    failure = Some(AcreSoundError::PendingSoundBytesExceeded {
                        limit: ACRE_MAX_PENDING_SOUND_ENCODED_BYTES,
                    });
                } else {
                    pending.encoded_bytes = next_item_bytes;
                    pending.last_update = now;
                    pending.chunks[slot] = Some(chunk.base64);
                    self.pending_encoded_bytes = next_total_bytes;
                    complete = pending.chunks.iter().all(Option::is_some);
                }
            }
        }

        if let Some(error) = failure {
            self.remove_pending(&id);
            return Err(error);
        }
        if !complete {
            return Ok(None);
        }

        let pending = self
            .remove_pending(&id)
            .expect("completed ACRE sound transfer remains present");
        let mut encoded = String::with_capacity(pending.encoded_bytes);
        for piece in pending.chunks {
            encoded.push_str(
                piece
                    .as_deref()
                    .expect("completed ACRE sound transfer contains all chunks"),
            );
        }
        let bytes = decode_base64(&encoded)?;
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        Ok(Some(AcreLoadedSound { id, bytes, sha256 }))
    }

    /// Drops incomplete transfers older than [`ACRE_SOUND_LOAD_TIMEOUT`] and
    /// returns their IDs for bounded diagnostics by the control plane.
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let expired = self
            .pending
            .iter()
            .filter(|(_, pending)| {
                now.saturating_duration_since(pending.last_update) >= ACRE_SOUND_LOAD_TIMEOUT
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &expired {
            let _ = self.remove_pending(id);
        }
        expired
    }

    /// Drops all incomplete encoded data, such as at a pipe disconnect/reset.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.pending_encoded_bytes = 0;
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn pending_encoded_bytes(&self) -> usize {
        self.pending_encoded_bytes
    }

    fn remove_pending(&mut self, id: &str) -> Option<PendingSound> {
        let pending = self.pending.remove(id)?;
        self.pending_encoded_bytes = self
            .pending_encoded_bytes
            .saturating_sub(pending.encoded_bytes);
        Some(pending)
    }
}

fn chunk_slot(index: usize, total: usize, zero_based: bool) -> Result<usize, AcreSoundError> {
    if zero_based {
        if index >= total {
            return Err(AcreSoundError::InvalidChunkIndex { index, total });
        }
        Ok(index)
    } else {
        if index == 0 || index > total {
            return Err(AcreSoundError::InvalidChunkIndex { index, total });
        }
        Ok(index - 1)
    }
}

fn expect_procedure(message: &AcreMessage, expected: &str) -> Result<(), AcreSoundError> {
    if message.procedure() == expected {
        Ok(())
    } else {
        Err(AcreSoundError::UnexpectedProcedure(
            message.procedure().to_owned(),
        ))
    }
}

fn expect_arity(message: &AcreMessage, expected: usize) -> Result<(), AcreSoundError> {
    if message.parameters().len() == expected {
        Ok(())
    } else {
        Err(AcreSoundError::WrongArity {
            procedure: message.procedure().to_owned(),
            expected,
            actual: message.parameters().len(),
        })
    }
}

fn validated_sound_id(value: &str) -> Result<String, AcreSoundError> {
    if value.is_empty() {
        return Err(AcreSoundError::EmptySoundId);
    }
    if value.len() > ACRE_MAX_SOUND_ID_BYTES {
        return Err(AcreSoundError::SoundIdTooLong {
            length: value.len(),
            limit: ACRE_MAX_SOUND_ID_BYTES,
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(AcreSoundError::InvalidSoundId);
    }
    Ok(value.to_owned())
}

fn parse_usize(message: &AcreMessage, index: usize) -> Result<usize, AcreSoundError> {
    message.parameters()[index]
        .parse::<usize>()
        .map_err(|_| AcreSoundError::InvalidInteger {
            procedure: message.procedure().to_owned(),
            index,
        })
}

fn parse_vector(message: &AcreMessage, index: usize) -> Result<AcreSpeakerVector, AcreSoundError> {
    Ok(AcreSpeakerVector {
        x: parse_finite_f32(message, index)?,
        z: parse_finite_f32(message, index + 1)?,
        y: parse_finite_f32(message, index + 2)?,
    })
}

fn parse_finite_f32(message: &AcreMessage, index: usize) -> Result<f32, AcreSoundError> {
    let value =
        message.parameters()[index]
            .parse::<f32>()
            .map_err(|_| AcreSoundError::InvalidFloat {
                procedure: message.procedure().to_owned(),
                index,
            })?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AcreSoundError::InvalidFloat {
            procedure: message.procedure().to_owned(),
            index,
        })
    }
}

fn decode_base64(encoded: &str) -> Result<Vec<u8>, AcreSoundError> {
    if encoded.is_empty() {
        return Err(AcreSoundError::EmptySoundPayload);
    }
    if !encoded.len().is_multiple_of(4) {
        return Err(AcreSoundError::InvalidBase64);
    }
    let max_decoded = (encoded.len() / 4) * 3;
    if max_decoded > ACRE_MAX_SOUND_DECODED_BYTES {
        return Err(AcreSoundError::DecodedSoundTooLarge {
            length: max_decoded,
            limit: ACRE_MAX_SOUND_DECODED_BYTES,
        });
    }

    let mut decoded = Vec::with_capacity(max_decoded);
    for (block_index, block) in encoded.as_bytes().chunks_exact(4).enumerate() {
        let last_block = block_index + 1 == encoded.len() / 4;
        let pad2 = block[2] == b'=';
        let pad3 = block[3] == b'=';
        if (pad2 && !pad3) || (pad3 && !last_block) {
            return Err(AcreSoundError::InvalidBase64);
        }
        let a = decode_base64_value(block[0]).ok_or(AcreSoundError::InvalidBase64)?;
        let b = decode_base64_value(block[1]).ok_or(AcreSoundError::InvalidBase64)?;
        let c = (!pad2)
            .then(|| decode_base64_value(block[2]).ok_or(AcreSoundError::InvalidBase64))
            .transpose()?
            .unwrap_or(0);
        let d = (!pad3)
            .then(|| decode_base64_value(block[3]).ok_or(AcreSoundError::InvalidBase64))
            .transpose()?
            .unwrap_or(0);

        decoded.push((a << 2) | (b >> 4));
        if !pad2 {
            decoded.push(((b & 0x0f) << 4) | (c >> 2));
        }
        if !pad3 {
            decoded.push(((c & 0x03) << 6) | d);
        }
    }
    if decoded.len() > ACRE_MAX_SOUND_DECODED_BYTES {
        return Err(AcreSoundError::DecodedSoundTooLarge {
            length: decoded.len(),
            limit: ACRE_MAX_SOUND_DECODED_BYTES,
        });
    }
    Ok(decoded)
}

const fn decode_base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some((byte - b'a') + 26),
        b'0'..=b'9' => Some((byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Validation or bounded-state error for ACRE's loaded-sound messages.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum AcreSoundError {
    #[error("unexpected ACRE sound procedure {0}")]
    UnexpectedProcedure(String),
    #[error("ACRE sound procedure {procedure} has {actual} parameters; expected {expected}")]
    WrongArity {
        procedure: String,
        expected: usize,
        actual: usize,
    },
    #[error("ACRE sound ID cannot be empty")]
    EmptySoundId,
    #[error("ACRE sound ID is {length} bytes; limit is {limit}")]
    SoundIdTooLong { length: usize, limit: usize },
    #[error("ACRE sound ID contains unsupported characters")]
    InvalidSoundId,
    #[error("ACRE sound procedure {procedure} parameter {index} must be an unsigned integer")]
    InvalidInteger { procedure: String, index: usize },
    #[error("ACRE sound procedure {procedure} parameter {index} must be a finite float")]
    InvalidFloat { procedure: String, index: usize },
    #[error("ACRE sound transfer declares {total} chunks; limit is {ACRE_MAX_SOUND_CHUNKS}")]
    InvalidChunkTotal { total: usize },
    #[error("ACRE sound chunk {index} is outside its declared total {total}")]
    InvalidChunkIndex { index: usize, total: usize },
    #[error("ACRE sound chunk is {length} bytes; limit is {limit}")]
    ChunkTooLong { length: usize, limit: usize },
    #[error("ACRE sound payload is not strict base64")]
    InvalidBase64,
    #[error("ACRE sound transfer {id} has an empty non-final chunk")]
    EmptyNonFinalChunk { id: String },
    #[error("ACRE sound transfer {id} changed its counter layout")]
    ConflictingChunkLayout { id: String },
    #[error("ACRE sound transfer {id} repeated a chunk with different data")]
    ConflictingChunk { id: String },
    #[error("ACRE sound transfer {id} is {length} encoded bytes; limit is {limit}")]
    EncodedSoundTooLarge {
        id: String,
        length: usize,
        limit: usize,
    },
    #[error("ACRE pending sound transfers exceed {limit} encoded bytes")]
    PendingSoundBytesExceeded { limit: usize },
    #[error("ACRE has more than {limit} incomplete sound transfers")]
    TooManyPendingSounds { limit: usize },
    #[error("ACRE sound payload is empty")]
    EmptySoundPayload,
    #[error("ACRE decoded sound is {length} bytes; limit is {limit}")]
    DecodedSoundTooLarge { length: usize, limit: usize },
    #[error("ACRE sound volume {volume} is outside 0..=1")]
    InvalidVolume { volume: f32 },
    #[error("ACRE sound world flag must be 0 or 1, got {0}")]
    InvalidWorldFlag(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> AcreMessage {
        AcreMessage::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn parses_the_v214_load_and_play_wire_shapes() {
        let chunk = parse_load_sound(&message("loadSound:ACRE_Click,1,2,QUJD,")).unwrap();
        assert_eq!(chunk.id(), "ACRE_Click");
        assert_eq!(chunk.index(), 1);
        assert_eq!(chunk.total(), 2);
        assert_eq!(chunk.base64(), "QUJD");

        let playback =
            parse_play_loaded_sound(&message("playLoadedSound:ACRE_Click,1,2,3,4,5,6,0.5,1,"))
                .unwrap();
        assert_eq!(playback.id(), "ACRE_Click");
        assert_eq!(
            playback.position(),
            AcreSpeakerVector {
                x: 1.0,
                z: 2.0,
                y: 3.0
            }
        );
        assert_eq!(
            playback.direction(),
            AcreSpeakerVector {
                x: 4.0,
                z: 5.0,
                y: 6.0
            }
        );
        assert!((playback.volume() - 0.5).abs() < f32::EPSILON);
        assert!(playback.is_world());
    }

    #[test]
    fn assembles_one_based_chunks_with_the_stock_terminal_empty_chunk() {
        let now = Instant::now();
        let mut assembler = AcreSoundAssembler::default();
        assert_eq!(
            assembler
                .accept(
                    parse_load_sound(&message("loadSound:ACRE_Click,1,3,QUJD,")).unwrap(),
                    now,
                )
                .unwrap(),
            None
        );
        assert_eq!(
            assembler
                .accept(
                    parse_load_sound(&message("loadSound:ACRE_Click,2,3,REVG,")).unwrap(),
                    now,
                )
                .unwrap(),
            None
        );
        let loaded = assembler
            .accept(
                parse_load_sound(&message("loadSound:ACRE_Click,3,3,,")).unwrap(),
                now,
            )
            .unwrap()
            .expect("final stock chunk completes the transfer");
        assert_eq!(loaded.id(), "ACRE_Click");
        assert_eq!(loaded.bytes(), b"ABCDEF");
        assert_eq!(loaded.sha256().len(), 32);
        assert_eq!(assembler.pending_count(), 0);
        assert_eq!(assembler.pending_encoded_bytes(), 0);
    }

    #[test]
    fn accepts_the_legacy_zero_based_fixture_shape() {
        let mut assembler = AcreSoundAssembler::default();
        let loaded = assembler
            .accept(
                parse_load_sound(&message("loadSound:fixture,0,1,QUJD,")).unwrap(),
                Instant::now(),
            )
            .unwrap()
            .expect("one zero-based chunk completes the fixture transfer");
        assert_eq!(loaded.bytes(), b"ABC");
    }

    #[test]
    fn conflicting_or_expired_transfers_are_discarded() {
        let now = Instant::now();
        let mut assembler = AcreSoundAssembler::default();
        let first = parse_load_sound(&message("loadSound:ACRE_Click,1,3,QUJD,")).unwrap();
        assert!(assembler.accept(first.clone(), now).unwrap().is_none());
        let conflict = parse_load_sound(&message("loadSound:ACRE_Click,1,3,REVG,")).unwrap();
        assert!(matches!(
            assembler.accept(conflict, now),
            Err(AcreSoundError::ConflictingChunk { .. })
        ));
        assert_eq!(assembler.pending_count(), 0);

        assert!(assembler.accept(first, now).unwrap().is_none());
        assert_eq!(
            assembler.expire(now + ACRE_SOUND_LOAD_TIMEOUT + Duration::from_millis(1)),
            vec!["ACRE_Click"]
        );
        assert_eq!(assembler.pending_count(), 0);
    }

    #[test]
    fn rejects_unsafe_ids_invalid_base64_and_out_of_range_playback() {
        assert!(matches!(
            parse_load_sound(&message("loadSound:../escape,1,1,QUJD,")),
            Err(AcreSoundError::InvalidSoundId)
        ));
        assert!(matches!(
            parse_load_sound(&message("loadSound:ACRE_Click,1,1,###,")),
            Err(AcreSoundError::InvalidBase64)
        ));
        assert!(matches!(
            parse_play_loaded_sound(&message("playLoadedSound:ACRE_Click,0,0,0,0,0,0,1.1,0,",)),
            Err(AcreSoundError::InvalidVolume { .. })
        ));
        assert!(matches!(
            parse_play_loaded_sound(&message("playLoadedSound:ACRE_Click,0,0,0,0,0,0,0.5,2,",)),
            Err(AcreSoundError::InvalidWorldFlag(_))
        ));
    }
}
