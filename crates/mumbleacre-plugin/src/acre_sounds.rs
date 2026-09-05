//! Non-RT preparation worker for ACRE2 `loadSound` / `playLoadedSound`.
//!
//! The ACRE pipe worker owns protocol assembly, while this worker decodes the
//! completed WAV, applies the requested bounded gain, and writes a temporary
//! sample for a Mumble plugin callback to pass to `playSample`. Mumble permits
//! API calls from worker threads but synchronizes them with its main thread, so
//! this worker deliberately never calls the API. Neither operation occurs in
//! `mumble_onAudioSourceFetched`. API v1.0 has no positional sample interface,
//! so non-centred or world playback is rejected instead of being silently
//! rendered with the wrong spatial meaning.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use mumbleacre_acre::{AcreLoadedSound, AcreSoundPlayback};

const MAX_QUEUED_SOUND_JOBS: usize = 32;
const MAX_CACHED_SOUNDS: usize = 32;
const MAX_CACHED_SAMPLE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ACTIVE_PLAYBACK_FILES: usize = 64;
const MAX_SOUND_DURATION: Duration = Duration::from_secs(30);
const PLAYBACK_FILE_GRACE: Duration = Duration::from_secs(5);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Owns the worker thread. Its cloneable [`AcreSoundWorkerClient`] is passed to
/// the pipe worker, which can submit bounded jobs and poll results without
/// touching files or Mumble APIs.
pub(crate) struct AcreSoundWorker {
    client: AcreSoundWorkerClient,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Bounded hand-off owned by the pipe worker.
#[derive(Clone)]
pub(crate) struct AcreSoundWorkerClient {
    jobs: SyncSender<AcreSoundWorkerJob>,
    results: Arc<Mutex<Receiver<AcreSoundWorkerResult>>>,
}

enum AcreSoundWorkerJob {
    Load {
        generation: u64,
        sound: AcreLoadedSound,
    },
    Play {
        generation: u64,
        request: AcreSoundPlayback,
    },
    Clear,
}

/// Outcome returned to the pipe/control boundary. Paths are ready only for a
/// short safe-retention window; the main Mumble control callback must invoke
/// `playSample` without handing them to the audio callback.
#[derive(Debug)]
pub(crate) enum AcreSoundWorkerResult {
    Loaded {
        generation: u64,
        id: String,
        sample_count: usize,
    },
    LoadFailed {
        generation: u64,
        id: String,
        reason: String,
    },
    PlaybackReady {
        generation: u64,
        id: String,
        path: PathBuf,
    },
    PlaybackFailed {
        generation: u64,
        id: String,
        reason: String,
    },
}

impl AcreSoundWorker {
    pub(crate) fn start() -> Result<Self, String> {
        let (jobs, receiver) = mpsc::sync_channel(MAX_QUEUED_SOUND_JOBS);
        let (result_sender, results) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("mumbleacre-acre-sounds".to_owned())
            .spawn(move || sound_worker_loop(receiver, result_sender, worker_stop))
            .map_err(|error| format!("could not start ACRE sound worker: {error}"))?;
        Ok(Self {
            client: AcreSoundWorkerClient {
                jobs,
                results: Arc::new(Mutex::new(results)),
            },
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn client(&self) -> AcreSoundWorkerClient {
        self.client.clone()
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AcreSoundWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

impl AcreSoundWorkerClient {
    pub(crate) fn enqueue_load(
        &self,
        generation: u64,
        sound: AcreLoadedSound,
    ) -> Result<(), &'static str> {
        self.try_enqueue(AcreSoundWorkerJob::Load { generation, sound })
    }

    pub(crate) fn enqueue_play(
        &self,
        generation: u64,
        request: AcreSoundPlayback,
    ) -> Result<(), &'static str> {
        self.try_enqueue(AcreSoundWorkerJob::Play {
            generation,
            request,
        })
    }

    /// Best-effort lifecycle boundary used after ACRE reset/disconnect. Active
    /// files are retained until Mumble has had enough time to consume them,
    /// but a later request can never resolve an old loaded-sound ID.
    pub(crate) fn enqueue_clear(&self) -> Result<(), &'static str> {
        self.try_enqueue(AcreSoundWorkerJob::Clear)
    }

    pub(crate) fn drain_results(&self) -> Vec<AcreSoundWorkerResult> {
        let receiver = self
            .results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        receiver.try_iter().collect()
    }

    fn try_enqueue(&self, job: AcreSoundWorkerJob) -> Result<(), &'static str> {
        match self.jobs.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("ACRE sound worker queue is full"),
            Err(TrySendError::Disconnected(_)) => Err("ACRE sound worker stopped"),
        }
    }
}

struct CachedSound {
    generation: u64,
    sha256: [u8; 32],
    spec: WavSpec,
    samples: Vec<i16>,
    duration: Duration,
}

struct PlaybackFile {
    path: PathBuf,
    expires_at: Instant,
}

// The thread owns these endpoints; borrowing them would not satisfy `spawn`'s
// `'static` boundary or make shutdown ownership explicit.
#[allow(clippy::needless_pass_by_value)]
fn sound_worker_loop(
    receiver: Receiver<AcreSoundWorkerJob>,
    result_sender: mpsc::Sender<AcreSoundWorkerResult>,
    stop: Arc<AtomicBool>,
) {
    let directory = acre_sound_directory();
    let mut cache = HashMap::<String, CachedSound>::with_capacity(MAX_CACHED_SOUNDS);
    let mut cached_sample_bytes = 0_usize;
    let mut playback_files = VecDeque::<PlaybackFile>::with_capacity(MAX_ACTIVE_PLAYBACK_FILES);
    let mut playback_nonce = 0_u64;

    while !stop.load(Ordering::Acquire) {
        cleanup_expired_playback_files(&mut playback_files, Instant::now());
        let Ok(job) = receiver.recv_timeout(WORKER_POLL_INTERVAL) else {
            continue;
        };
        let result = match job {
            AcreSoundWorkerJob::Load { generation, sound } => {
                prepare_loaded_sound(generation, &sound, &mut cache, &mut cached_sample_bytes)
            }
            AcreSoundWorkerJob::Play {
                generation,
                request,
            } => prepare_playback_file(
                generation,
                &request,
                &cache,
                &directory,
                &mut playback_files,
                &mut playback_nonce,
            ),
            AcreSoundWorkerJob::Clear => {
                cache.clear();
                cached_sample_bytes = 0;
                continue;
            }
        };
        // The producer queue is bounded and the pipe worker polls results at
        // least once per control tick. An unbounded result channel cannot be
        // driven by arbitrary audio callbacks and avoids losing a required
        // `handleLoadedSound` / `handleSoundError` reply under short bursts.
        let _ = result_sender.send(result);
    }
    cleanup_expired_playback_files(&mut playback_files, Instant::now() + PLAYBACK_FILE_GRACE);
}

fn prepare_loaded_sound(
    generation: u64,
    sound: &AcreLoadedSound,
    cache: &mut HashMap<String, CachedSound>,
    cached_sample_bytes: &mut usize,
) -> AcreSoundWorkerResult {
    let id = sound.id().to_owned();
    let parsed = match parse_acre_wav(sound.bytes()) {
        Ok(parsed) => parsed,
        Err(reason) => {
            return AcreSoundWorkerResult::LoadFailed {
                generation,
                id,
                reason,
            };
        }
    };
    let sample_bytes = parsed
        .samples
        .len()
        .saturating_mul(std::mem::size_of::<i16>());
    let replacing = cache.get(&id).map_or(0, |cached| {
        cached
            .samples
            .len()
            .saturating_mul(std::mem::size_of::<i16>())
    });
    let next_total = cached_sample_bytes
        .saturating_sub(replacing)
        .saturating_add(sample_bytes);
    if (cache.len() >= MAX_CACHED_SOUNDS && !cache.contains_key(&id))
        || next_total > MAX_CACHED_SAMPLE_BYTES
    {
        return AcreSoundWorkerResult::LoadFailed {
            generation,
            id,
            reason: "ACRE prepared-sound cache is full".to_owned(),
        };
    }

    let sample_count = parsed.samples.len();
    *cached_sample_bytes = next_total;
    cache.insert(
        id.clone(),
        CachedSound {
            generation,
            sha256: *sound.sha256(),
            spec: parsed.spec,
            samples: parsed.samples,
            duration: parsed.duration,
        },
    );
    AcreSoundWorkerResult::Loaded {
        generation,
        id,
        sample_count,
    }
}

fn prepare_playback_file(
    generation: u64,
    request: &AcreSoundPlayback,
    cache: &HashMap<String, CachedSound>,
    directory: &Path,
    playback_files: &mut VecDeque<PlaybackFile>,
    playback_nonce: &mut u64,
) -> AcreSoundWorkerResult {
    let id = request.id().to_owned();
    if !is_supported_local_sample(request) {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason: "Mumble API v1.0 only supports centred local ACRE sounds".to_owned(),
        };
    }
    let Some(sound) = cache.get(&id) else {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason: "ACRE sound was not prepared".to_owned(),
        };
    };
    if sound.generation != generation {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason: "ACRE sound belongs to a previous control generation".to_owned(),
        };
    }
    cleanup_expired_playback_files(playback_files, Instant::now());
    if playback_files.len() >= MAX_ACTIVE_PLAYBACK_FILES {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason: "too many active ACRE sound samples".to_owned(),
        };
    }
    if let Err(error) = fs::create_dir_all(directory) {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason: format!("could not create ACRE sound directory: {error}"),
        };
    }
    *playback_nonce = playback_nonce.wrapping_add(1);
    let path = directory.join(format!(
        "{}-{:016x}.wav",
        short_hash(&sound.sha256),
        *playback_nonce
    ));
    if let Err(reason) = write_scaled_wav(&path, sound, request.volume()) {
        return AcreSoundWorkerResult::PlaybackFailed {
            generation,
            id,
            reason,
        };
    }
    playback_files.push_back(PlaybackFile {
        path: path.clone(),
        expires_at: Instant::now() + sound.duration + PLAYBACK_FILE_GRACE,
    });
    AcreSoundWorkerResult::PlaybackReady {
        generation,
        id,
        path,
    }
}

fn is_supported_local_sample(request: &AcreSoundPlayback) -> bool {
    let position = request.position();
    let direction = request.direction();
    !request.is_world()
        && position.x == 0.0
        && position.z == 0.0
        && position.y == 0.0
        && direction.x == 0.0
        && direction.z == 0.0
        && direction.y == 0.0
}

struct ParsedWav {
    spec: WavSpec,
    samples: Vec<i16>,
    duration: Duration,
}

fn parse_acre_wav(bytes: &[u8]) -> Result<ParsedWav, String> {
    let mut reader = WavReader::new(Cursor::new(bytes))
        .map_err(|error| format!("ACRE sound is not a readable WAV: {error}"))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_format != SampleFormat::Int
        || spec.bits_per_sample != 16
        || spec.sample_rate != 48_000
    {
        return Err("ACRE sound must be mono PCM16 at 48 kHz".to_owned());
    }
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("ACRE WAV sample decode failed: {error}"))?;
    if samples.is_empty() {
        return Err("ACRE sound has no PCM samples".to_owned());
    }
    let sample_count = u32::try_from(samples.len())
        .map_err(|_| "ACRE sound has too many PCM samples".to_owned())?;
    let duration = Duration::from_secs_f64(f64::from(sample_count) / f64::from(spec.sample_rate));
    if duration > MAX_SOUND_DURATION {
        return Err("ACRE sound exceeds the 30 second duration limit".to_owned());
    }
    Ok(ParsedWav {
        spec,
        samples,
        duration,
    })
}

fn write_scaled_wav(path: &Path, sound: &CachedSound, volume: f32) -> Result<(), String> {
    let mut writer = WavWriter::create(path, sound.spec)
        .map_err(|error| format!("could not create prepared ACRE WAV: {error}"))?;
    for sample in &sound.samples {
        writer
            .write_sample(scale_sample(*sample, volume))
            .map_err(|error| format!("could not write prepared ACRE WAV: {error}"))?;
    }
    writer
        .finalize()
        .map_err(|error| format!("could not finalize prepared ACRE WAV: {error}"))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]
fn scale_sample(sample: i16, volume: f32) -> i16 {
    (f32::from(sample) * volume)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn cleanup_expired_playback_files(files: &mut VecDeque<PlaybackFile>, now: Instant) {
    while files.front().is_some_and(|file| now >= file.expires_at) {
        if let Some(file) = files.pop_front() {
            let _ = fs::remove_file(file.path);
        }
    }
}

fn acre_sound_directory() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("MumbleACRE")
            .join("sounds")
            .join("acre");
    }
    if let Some(xdg_state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(xdg_state)
            .join("mumbleacre")
            .join("sounds")
            .join("acre");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("mumbleacre")
            .join("sounds")
            .join("acre");
    }
    std::env::temp_dir()
        .join("MumbleACRE")
        .join("sounds")
        .join("acre")
}

fn short_hash(hash: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(16);
    for byte in &hash[..8] {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use mumbleacre_acre::{
        AcreMessage, AcreSoundAssembler, parse_load_sound, parse_play_loaded_sound,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn loaded_sound(bytes: &[u8]) -> AcreLoadedSound {
        let encoded = encode_base64(bytes);
        let mut assembler = AcreSoundAssembler::default();
        assembler
            .accept(
                parse_load_sound(
                    &AcreMessage::parse(format!("loadSound:ACRE_Test,0,1,{encoded},").as_bytes())
                        .unwrap(),
                )
                .unwrap(),
                Instant::now(),
            )
            .unwrap()
            .unwrap()
    }

    fn pcm_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in bytes.chunks(3) {
            let first = chunk[0];
            let second = *chunk.get(1).unwrap_or(&0);
            let third = *chunk.get(2).unwrap_or(&0);
            encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
            encoded.push(char::from(
                ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
            ));
            encoded.push(if chunk.len() > 1 {
                char::from(ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))])
            } else {
                '='
            });
            encoded.push(if chunk.len() > 2 {
                char::from(ALPHABET[usize::from(third & 0x3f)])
            } else {
                '='
            });
        }
        encoded
    }

    #[test]
    fn parses_only_the_wav_shape_generated_by_acre_v214() {
        let valid = pcm_wav(48_000, 1, &[0, 1_000, -1_000]);
        let parsed = parse_acre_wav(&valid).unwrap();
        assert_eq!(parsed.samples, [0, 1_000, -1_000]);
        assert_eq!(parsed.spec.sample_rate, 48_000);
        assert!(parse_acre_wav(&pcm_wav(22_050, 1, &[0])).is_err());
        assert!(parse_acre_wav(&pcm_wav(48_000, 2, &[0, 0])).is_err());
    }

    #[test]
    fn applies_requested_volume_without_clipping() {
        assert_eq!(scale_sample(20_000, 0.5), 10_000);
        assert_eq!(scale_sample(-20_000, 0.5), -10_000);
        assert_eq!(scale_sample(i16::MAX, 1.0), i16::MAX);
    }

    #[test]
    fn prepared_sound_cache_rejects_non_wav_payloads_without_exposing_bytes() {
        let sound = loaded_sound(b"not-a-wav");
        let mut cache = HashMap::new();
        let mut cached_bytes = 0;
        assert!(matches!(
            prepare_loaded_sound(0, &sound, &mut cache, &mut cached_bytes),
            AcreSoundWorkerResult::LoadFailed { .. }
        ));
        assert!(cache.is_empty());
    }

    #[test]
    fn prepares_a_centred_local_sample_with_requested_gain_off_the_rt_path() {
        let sound = loaded_sound(&pcm_wav(48_000, 1, &[20_000, -20_000]));
        let mut cache = HashMap::new();
        let mut cached_bytes = 0;
        assert!(matches!(
            prepare_loaded_sound(7, &sound, &mut cache, &mut cached_bytes),
            AcreSoundWorkerResult::Loaded {
                generation: 7,
                sample_count: 2,
                ..
            }
        ));

        let request = parse_play_loaded_sound(
            &AcreMessage::parse(b"playLoadedSound:ACRE_Test,0,0,0,0,0,0,0.5,0,").unwrap(),
        )
        .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mumbleacre-acre-sounds-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        let mut playback_files = VecDeque::new();
        let mut playback_nonce = 0;
        let result = prepare_playback_file(
            7,
            &request,
            &cache,
            &directory,
            &mut playback_files,
            &mut playback_nonce,
        );
        let path = match result {
            AcreSoundWorkerResult::PlaybackReady {
                generation: 7,
                path,
                ..
            } => path,
            other => panic!("expected prepared local sample, got {other:?}"),
        };
        let prepared = parse_acre_wav(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(prepared.samples, [10_000, -10_000]);

        cleanup_expired_playback_files(
            &mut playback_files,
            Instant::now() + MAX_SOUND_DURATION + PLAYBACK_FILE_GRACE,
        );
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_world_or_positioned_requests_instead_of_losing_spatial_meaning() {
        let centred = parse_play_loaded_sound(
            &AcreMessage::parse(b"playLoadedSound:ACRE_Click,0,0,0,0,0,0,1,0,").unwrap(),
        )
        .unwrap();
        assert!(is_supported_local_sample(&centred));

        let world = parse_play_loaded_sound(
            &AcreMessage::parse(b"playLoadedSound:ACRE_Click,0,0,0,0,0,0,1,1,").unwrap(),
        )
        .unwrap();
        assert!(!is_supported_local_sample(&world));

        let positioned = parse_play_loaded_sound(
            &AcreMessage::parse(b"playLoadedSound:ACRE_Click,1,0,0,0,0,0,1,0,").unwrap(),
        )
        .unwrap();
        assert!(!is_supported_local_sample(&positioned));
    }
}
