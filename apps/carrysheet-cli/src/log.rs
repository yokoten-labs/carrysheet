//! The MCP server's log: one line per event, to stderr or `--log-file`,
//! never to stdout. The home folder is shown as `~`, and control characters
//! from clients are escaped, so a log line cannot be forged or hide escape
//! codes.
//!
//! Lines are written by a thread of their own behind a bounded queue, so a
//! slow stderr or disk never stalls the server. When the queue is full, lines
//! are dropped and counted, and the count is logged once there is room.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use carrysheet_commands::paths::{display_path, redact_home};
use carrysheet_commands::{CommandError, ErrorCode};

/// How many lines may wait for the writer before new ones are dropped.
const QUEUE: usize = 1024;

/// Where log lines go.
pub struct Log {
    queue: Mutex<Option<SyncSender<String>>>,
    dropped: Arc<AtomicU64>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl Log {
    /// Logs to stderr.
    pub fn stderr() -> Log {
        Log::start(Box::new(std::io::stderr()))
    }

    /// Appends to `path`, creating it if needed.
    pub fn file(path: &Path) -> Result<Log, CommandError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                CommandError::new(
                    ErrorCode::Io,
                    format!("cannot open the log file {}", display_path(path)),
                    e.to_string(),
                    "Give --log-file a path in a folder you can write to, or leave it out to log to stderr.",
                )
            })?;
        Ok(Log::start(Box::new(file)))
    }

    fn start(mut out: Box<dyn Write + Send>) -> Log {
        let (tx, rx) = sync_channel::<String>(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let counter = dropped.clone();
        let report = move |out: &mut Box<dyn Write + Send>| {
            let n = counter.swap(0, Ordering::SeqCst);
            if n > 0 {
                let line = stamp(&format!(
                    "{n} log lines were dropped: the log could not keep up"
                ));
                let _ = out.write_all(line.as_bytes());
            }
        };
        // If no thread can be started, lines are dropped; the server still runs.
        let writer = std::thread::Builder::new()
            .name("carrysheet-log".into())
            .spawn(move || {
                for line in rx {
                    report(&mut out);
                    let _ = out.write_all(line.as_bytes());
                    let _ = out.flush();
                }
                report(&mut out);
                let _ = out.flush();
            })
            .ok();
        Log {
            queue: Mutex::new(Some(tx)),
            dropped,
            writer: Mutex::new(writer),
        }
    }

    /// Queues one line with a UTC timestamp. Never blocks on the output.
    pub fn line(&self, message: &str) {
        let line = stamp(message);
        let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = queue.as_ref()
            && let Err(TrySendError::Full(_)) = tx.try_send(line)
        {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Writes out every queued line and stops the writer. Lines logged
    /// afterwards are dropped.
    pub fn close(&self) {
        drop(self.queue.lock().unwrap_or_else(|e| e.into_inner()).take());
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.close();
    }
}

/// One finished log line: timestamp, escaped message, newline.
fn stamp(message: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!(
        "{} carrysheet-cli: {}\n",
        utc_timestamp(secs),
        escape(&redact_home(message))
    )
}

/// Escapes control characters (CR, LF, ESC and the rest) and the Unicode
/// characters that reorder text, so client text stays on one plain line.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control()
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since 1970 (UTC, proleptic Gregorian).
pub fn utc_timestamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Days to a civil date: the era-based method published by Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Sends warnings and errors that rmcp reports through `tracing` to the log,
/// so problems in the transport are not silent. Spans are ignored.
pub struct TracingToLog(pub Arc<Log>);

impl tracing::Subscriber for TracingToLog {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= tracing::Level::WARN
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        let meta = event.metadata();
        self.0
            .line(&format!("{} {}:{}", meta.level(), meta.target(), fields.0));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Collects an event's fields as ` message key=value ...`.
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_dates() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951_825_600), "2000-02-29T12:00:00Z");
        assert_eq!(utc_timestamp(1_792_454_399), "2026-10-19T23:59:59Z");
        assert_eq!(utc_timestamp(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn log_file_gets_lines_without_the_home_folder() {
        let path = std::env::temp_dir().join(format!("carrysheet-log-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let log = Log::file(&path).unwrap();
        log.line("first\nline");
        if let Some(home) = carrysheet_commands::paths::home_dir() {
            log.line(&format!("opened {}", home.join("a.xlsx").display()));
        }
        log.line("forged\r\n2026-01-01T00:00:00Z carrysheet-cli: \u{1b}[31mred");
        log.close();
        let text = std::fs::read_to_string(&path).unwrap();
        // Every message stays on one line, whatever it holds.
        assert!(
            text.lines().all(|l| l.contains(" carrysheet-cli: ")),
            "{text}"
        );
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\r'));
        assert!(text.contains(r"forged\r\n2026"), "{text}");
        let _ = std::fs::remove_file(&path);
        let first = text.lines().next().unwrap();
        assert!(first.ends_with(r" carrysheet-cli: first\nline"), "{first}");
        if let Some(home) = carrysheet_commands::paths::home_dir() {
            assert!(!text.contains(&home.display().to_string()), "{text}");
        }
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(escape("a\tb\nc\rd"), r"a\tb\nc\rd");
        assert_eq!(escape("\u{1b}[0m\u{7f}"), r"\u{1b}[0m\u{7f}");
        assert_eq!(escape("abc\u{202e}cba"), r"abc\u{202e}cba");
        assert_eq!(escape("表格 ok"), "表格 ok");
    }

    #[test]
    fn unwritable_log_file_is_a_clear_error() {
        let e = Log::file(Path::new("/no/such/folder/x.log")).err().unwrap();
        assert_eq!(e.code, ErrorCode::Io);
        assert!(e.fix.contains("--log-file"));
    }
}
