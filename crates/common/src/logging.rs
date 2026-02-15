use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Noop,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_directive(self) -> &'static str {
        match self {
            Self::Noop => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    pub fn allows(self, event: Self) -> bool {
        let current = self.rank();
        let required = event.rank();
        current >= required && current > 0
    }

    fn rank(self) -> u8 {
        match self {
            Self::Noop => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

#[derive(Clone, Debug)]
pub struct LoggingConfig {
    pub level: LogLevel,
    pub log_file: Option<PathBuf>,
    pub rotate_keep: usize,
    pub rotate_max_bytes: u64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            log_file: None,
            rotate_keep: 5,
            rotate_max_bytes: 10 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("logging init failed: {0}")]
    Init(String),
    #[error("logging io failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct LoggingHandle {
    filter: reload::Handle<EnvFilter, tracing_subscriber::Registry>,
    rotating_file: Option<Arc<RotatingFile>>,
}

impl LoggingHandle {
    pub fn set_level(&self, level: LogLevel) -> Result<(), LoggingError> {
        self.filter
            .modify(|filter| {
                *filter = EnvFilter::new(level.as_directive());
            })
            .map_err(|err| LoggingError::Init(format!("reload filter failed: {err}")))
    }

    pub fn rotate_logs(&self) -> Result<(), LoggingError> {
        if let Some(rotating_file) = &self.rotating_file {
            rotating_file.rotate_now()?;
        }
        Ok(())
    }
}

pub fn init_logging(config: LoggingConfig) -> Result<LoggingHandle, LoggingError> {
    let env_filter = EnvFilter::new(config.level.as_directive());
    let (filter_layer, filter_handle) = reload::Layer::new(env_filter);

    let rotating_file = if let Some(path) = config.log_file {
        Some(Arc::new(RotatingFile::open(
            path,
            config.rotate_keep,
            config.rotate_max_bytes,
        )?))
    } else {
        None
    };

    if let Some(rotating_file) = rotating_file.as_ref() {
        let writer = RotatingMakeWriter {
            rotating_file: rotating_file.clone(),
        };
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            )
            .try_init()
            .map_err(|err| LoggingError::Init(err.to_string()))?;
    } else {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|err| LoggingError::Init(err.to_string()))?;
    }

    Ok(LoggingHandle {
        filter: filter_handle,
        rotating_file,
    })
}

#[derive(Clone)]
struct RotatingMakeWriter {
    rotating_file: Arc<RotatingFile>,
}

impl<'a> MakeWriter<'a> for RotatingMakeWriter {
    type Writer = RotatingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingWriter {
            rotating_file: self.rotating_file.clone(),
        }
    }
}

struct RotatingWriter {
    rotating_file: Arc<RotatingFile>,
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rotating_file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.rotating_file.flush()
    }
}

struct RotatingFile {
    state: Mutex<RotatingFileState>,
    rotate_keep: usize,
    rotate_max_bytes: u64,
}

struct RotatingFileState {
    file_path: PathBuf,
    file: File,
    current_size: u64,
    day_key: u64,
}

impl RotatingFile {
    fn open(file_path: PathBuf, rotate_keep: usize, rotate_max_bytes: u64) -> io::Result<Self> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        let size = file.metadata()?.len();
        let day_key = current_day_key();

        Ok(Self {
            state: Mutex::new(RotatingFileState {
                file_path,
                file,
                current_size: size,
                day_key,
            }),
            rotate_keep,
            rotate_max_bytes,
        })
    }

    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("log mutex poisoned"))?;

        let should_rotate = self.needs_rotation(&state, buf.len() as u64);
        if should_rotate {
            self.rotate_locked(&mut state)?;
        }

        let written = state.file.write(buf)?;
        state.current_size = state.current_size.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("log mutex poisoned"))?;
        state.file.flush()
    }

    fn rotate_now(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("log mutex poisoned"))?;
        self.rotate_locked(&mut state)
    }

    fn needs_rotation(&self, state: &RotatingFileState, incoming_bytes: u64) -> bool {
        if current_day_key() != state.day_key {
            return true;
        }
        state.current_size.saturating_add(incoming_bytes) > self.rotate_max_bytes
    }

    fn rotate_locked(&self, state: &mut RotatingFileState) -> io::Result<()> {
        state.file.flush()?;

        let now = current_unix_seconds();
        let rotated_path = rotated_file_path(&state.file_path, now);
        let _ = std::fs::remove_file(&rotated_path);
        if state.file_path.exists() {
            std::fs::rename(&state.file_path, &rotated_path)?;
            compress_to_gzip(&rotated_path)?;
        }

        state.file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&state.file_path)?;
        state.current_size = 0;
        state.day_key = current_day_key();

        cleanup_rotated_logs(&state.file_path, self.rotate_keep)?;
        Ok(())
    }
}

fn rotated_file_path(base: &Path, unix_seconds: u64) -> PathBuf {
    PathBuf::from(format!("{}.{}", base.to_string_lossy(), unix_seconds))
}

fn compress_to_gzip(path: &Path) -> io::Result<()> {
    let gzip_path = PathBuf::from(format!("{}.gz", path.to_string_lossy()));
    let input = std::fs::read(path)?;
    let output_file = File::create(&gzip_path)?;
    let mut encoder = GzEncoder::new(output_file, Compression::default());
    encoder.write_all(&input)?;
    encoder.finish()?;
    std::fs::remove_file(path)?;
    Ok(())
}

fn cleanup_rotated_logs(base_file: &Path, keep: usize) -> io::Result<()> {
    if keep == 0 {
        return Ok(());
    }
    let Some(parent) = base_file.parent() else {
        return Ok(());
    };
    let file_name = match base_file.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => return Ok(()),
    };

    let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with(&file_name))
            .unwrap_or(false)
        {
            continue;
        }
        if !path.to_string_lossy().ends_with(".gz") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        files.push((path, modified));
    }

    files.sort_by(|a, b| b.1.cmp(&a.1));
    for (index, (path, _)) in files.into_iter().enumerate() {
        if index >= keep {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn current_day_key() -> u64 {
    current_unix_seconds() / 86_400
}

#[cfg(test)]
mod logging_test {
    use super::LogLevel;

    #[test]
    fn info_allows_warn_and_error_but_not_debug() {
        assert!(LogLevel::Info.allows(LogLevel::Warn));
        assert!(LogLevel::Info.allows(LogLevel::Error));
        assert!(!LogLevel::Info.allows(LogLevel::Debug));
    }

    #[test]
    fn noop_disables_all_events() {
        assert!(!LogLevel::Noop.allows(LogLevel::Error));
        assert!(!LogLevel::Noop.allows(LogLevel::Trace));
    }

    #[test]
    fn trace_allows_everything() {
        assert!(LogLevel::Trace.allows(LogLevel::Error));
        assert!(LogLevel::Trace.allows(LogLevel::Trace));
    }
}
