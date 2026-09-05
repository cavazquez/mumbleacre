// SPDX-License-Identifier: GPL-3.0

//! Shared human-readable structured logging for MumbleACRE runtime components.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_BACKUPS: usize = 3;
static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    #[must_use]
    pub const fn allows(self, event: Self) -> bool {
        event as u8 <= self as u8
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LevelConfig {
    pub level: LogLevel,
    pub invalid_value: Option<String>,
}

impl LevelConfig {
    #[must_use]
    pub fn from_value(value: Option<&str>) -> Self {
        match value {
            None => Self {
                level: LogLevel::Info,
                invalid_value: None,
            },
            Some(value) => LogLevel::parse(value).map_or_else(
                || Self {
                    level: LogLevel::Info,
                    invalid_value: Some(value.to_owned()),
                },
                |level| Self {
                    level,
                    invalid_value: None,
                },
            ),
        }
    }

    #[must_use]
    pub fn from_environment() -> Self {
        Self::from_value(std::env::var("MumbleACRE_LOG").ok().as_deref())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RotationPolicy {
    pub max_bytes: u64,
    pub backups: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_LOG_BYTES,
            backups: DEFAULT_BACKUPS,
        }
    }
}

pub struct EventLog {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    component: String,
    max_level: LogLevel,
    run_id: String,
    policy: RotationPolicy,
    current_bytes: u64,
}

impl EventLog {
    pub fn open(
        path: impl Into<PathBuf>,
        component: &str,
        max_level: LogLevel,
    ) -> io::Result<Self> {
        let run_id = new_run_id();
        Self::open_with_policy(
            path,
            component,
            max_level,
            &run_id,
            RotationPolicy::default(),
        )
    }

    pub fn open_with_policy(
        path: impl Into<PathBuf>,
        component: &str,
        max_level: LogLevel,
        run_id: &str,
        policy: RotationPolicy,
    ) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= policy.max_bytes) {
            rotate_files(&path, policy.backups)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let current_bytes = file.metadata()?.len();
        Ok(Self {
            path,
            writer: Some(BufWriter::new(file)),
            component: clean_token(component),
            max_level,
            run_id: clean_token(run_id),
            policy,
            current_bytes,
        })
    }

    pub fn write(&mut self, level: LogLevel, event: &str, message: &str) -> io::Result<bool> {
        if !self.max_level.allows(level) {
            return Ok(false);
        }
        let line = format_event_line(
            unix_ms(),
            level,
            &self.component,
            &self.run_id,
            event,
            message,
        );
        let line_bytes = u64::try_from(line.len() + 1).unwrap_or(u64::MAX);
        if self.current_bytes > 0
            && self.current_bytes.saturating_add(line_bytes) > self.policy.max_bytes
        {
            self.rotate()?;
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("event log writer is unavailable"))?;
        writeln!(writer, "{line}")?;
        writer.flush()?;
        self.current_bytes = self.current_bytes.saturating_add(line_bytes);
        Ok(true)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.as_mut().map_or(Ok(()), Write::flush)
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
        }
        rotate_files(&self.path, self.policy.backups)?;
        self.writer = Some(BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        ));
        self.current_bytes = 0;
        Ok(())
    }
}

#[must_use]
pub fn format_event_line(
    timestamp_unix_ms: u128,
    level: LogLevel,
    component: &str,
    run_id: &str,
    event: &str,
    message: &str,
) -> String {
    format!(
        "timestamp_unix_ms={timestamp_unix_ms} level={} component={} run_id={} event={} message={}",
        level.as_str(),
        clean_token(component),
        clean_token(run_id),
        clean_token(event),
        clean_message(message)
    )
}

#[must_use]
pub fn new_run_id() -> String {
    let sequence = RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{sequence}", std::process::id(), unix_ms())
}

#[must_use]
pub fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

fn rotate_files(path: &Path, backups: usize) -> io::Result<()> {
    if backups == 0 {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    let oldest = backup_path(path, backups);
    if oldest.exists() {
        fs::remove_file(oldest)?;
    }
    for index in (1..backups).rev() {
        let source = backup_path(path, index);
        if source.exists() {
            fs::rename(source, backup_path(path, index + 1))?;
        }
    }
    if path.exists() {
        fs::rename(path, backup_path(path, 1))?;
    }
    Ok(())
}

fn clean_token(value: &str) -> String {
    let clean: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "unknown".to_owned()
    } else {
        clean
    }
}

fn clean_message(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| {
            if matches!(character, '\r' | '\n' | '\t') {
                ' '
            } else if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect()
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mumbleacre-logging-{name}-{}", new_run_id()))
    }

    #[test]
    fn levels_parse_and_filter_consistently() {
        assert_eq!(LogLevel::parse(" DEBUG "), Some(LogLevel::Debug));
        assert!(LogLevel::Info.allows(LogLevel::Warn));
        assert!(!LogLevel::Info.allows(LogLevel::Debug));
        assert_eq!(
            LevelConfig::from_value(Some("verbose")),
            LevelConfig {
                level: LogLevel::Info,
                invalid_value: Some("verbose".to_owned())
            }
        );
    }

    #[test]
    fn format_is_single_line_and_has_stable_fields() {
        let line = format_event_line(
            123,
            LogLevel::Warn,
            "plug in",
            "run=1",
            "bad event",
            "first\r\nsecond\tthird",
        );

        assert_eq!(
            line,
            "timestamp_unix_ms=123 level=warn component=plug_in run_id=run_1 event=bad_event message=first  second third"
        );
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn filtered_events_are_not_written() {
        let directory = test_directory("filter");
        let path = directory.join("component.log");
        let mut log = EventLog::open_with_policy(
            &path,
            "test",
            LogLevel::Info,
            "run-filter",
            RotationPolicy::default(),
        )
        .unwrap();

        assert!(
            !log.write(LogLevel::Debug, "audio_decision", "hidden")
                .unwrap()
        );
        assert!(log.write(LogLevel::Info, "started", "visible").unwrap());
        log.flush().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("hidden"));
        assert!(contents.contains("event=started message=visible"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotation_keeps_configured_backups() {
        let directory = test_directory("rotation");
        let path = directory.join("component.log");
        let policy = RotationPolicy {
            max_bytes: 170,
            backups: 2,
        };
        let mut log =
            EventLog::open_with_policy(&path, "test", LogLevel::Trace, "run-rotation", policy)
                .unwrap();

        for index in 0..8 {
            log.write(LogLevel::Info, "rotation", &format!("entry={index}"))
                .unwrap();
        }
        log.flush().unwrap();

        assert!(path.exists());
        assert!(backup_path(&path, 1).exists());
        assert!(backup_path(&path, 2).exists());
        assert!(!backup_path(&path, 3).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn each_log_instance_gets_a_distinct_safe_run_id() {
        let first = new_run_id();
        let second = new_run_id();

        assert_ne!(first, second);
        assert!(first.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        }));
    }
}
