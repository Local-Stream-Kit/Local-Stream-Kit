//! The log: to the console and to a file a day in `logs/`
//! (`lsk.2026-10-13.log`, local dates, the last [`KEEP_DAYS`] kept), at
//! the level the Detailed log setting picks (`RUST_LOG`, when set, decides
//! instead), with the latest warnings and errors kept for the Status page.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDate};
use protocol::panel::{LogLine, LogStatus};
use tokio::sync::watch;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt, reload};

/// The log's folder, next to `lsk.json`.
pub const FOLDER: &str = "logs";
/// Days of files kept; older ones are deleted when a new day's starts.
const KEEP_DAYS: usize = 7;
/// Warnings and errors kept for the Status page.
const RECENT: usize = 20;
const NORMAL: &str = "info";
const DETAILED: &str = "info,lsk=debug";
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

#[derive(Clone)]
pub struct Logging {
    inner: Arc<Inner>,
}

struct Inner {
    filter: reload::Handle<EnvFilter, Registry>,
    folder: PathBuf,
    rust_log: Option<String>,
    detailed: Mutex<bool>,
    recent: Arc<Recent>,
}

impl Logging {
    /// Starts logging into `folder` (made when the first line is written),
    /// not detailed until [`Logging::set_detailed`]. Panics are logged too.
    pub fn start(folder: &Path) -> Logging {
        let rust_log = std::env::var("RUST_LOG").ok().filter(|_| EnvFilter::try_from_default_env().is_ok());
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(NORMAL));
        let (filter, handle) = reload::Layer::new(filter);
        let recent = Arc::new(Recent::default());
        let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        Registry::default()
            .with(filter)
            .with(fmt::layer().with_timer(ChronoLocal::new(TIME_FORMAT.into())))
            .with(
                fmt::layer()
                    .with_timer(ChronoLocal::new(TIME_FORMAT.into()))
                    .with_ansi(false)
                    .with_writer(DailyFile::new(folder.clone())),
            )
            .with(RecentLayer(Arc::clone(&recent)))
            .init();
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            tracing::error!("{info}");
            default_hook(info);
        }));
        Logging {
            inner: Arc::new(Inner { filter: handle, folder, rust_log, detailed: Mutex::new(false), recent }),
        }
    }

    /// Debug lines too, or not. Does nothing while `RUST_LOG` is set.
    pub fn set_detailed(&self, detailed: bool) {
        if self.inner.rust_log.is_some() {
            return;
        }
        {
            let mut current = self.inner.detailed.lock().unwrap();
            if *current == detailed {
                return;
            }
            *current = detailed;
        }
        let filter = EnvFilter::new(if detailed { DETAILED } else { NORMAL });
        if let Err(error) = self.inner.filter.reload(filter) {
            tracing::error!("couldn't change what's logged: {error}");
            return;
        }
        tracing::info!("detailed log {}", if detailed { "on" } else { "off" });
    }

    pub fn folder(&self) -> &Path {
        &self.inner.folder
    }

    /// Bumped whenever the recent warnings and errors change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.recent.changed.subscribe()
    }

    pub fn status(&self) -> LogStatus {
        LogStatus {
            folder: self.inner.folder.display().to_string(),
            rust_log: self.inner.rust_log.clone(),
            recent: self
                .inner
                .recent
                .lines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .cloned()
                .collect(),
        }
    }

    /// Forgets the recent warnings and errors (the files keep them).
    pub fn clear_recent(&self) {
        self.inner.recent.lines.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.inner.recent.changed.send_modify(|version| *version += 1);
    }
}

/// The latest warnings and errors, newest first.
struct Recent {
    lines: Mutex<VecDeque<LogLine>>,
    changed: watch::Sender<u64>,
}

impl Default for Recent {
    fn default() -> Recent {
        Recent { lines: Mutex::default(), changed: watch::Sender::new(0) }
    }
}

impl Recent {
    /// Adds a line, or counts it again if it's the same as the newest.
    fn push(&self, at_ms: u64, error: bool, text: String) {
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        match lines.front_mut() {
            Some(newest) if newest.text == text && newest.error == error => {
                newest.count += 1;
                newest.at_ms = at_ms;
            }
            _ => {
                lines.push_front(LogLine { at_ms, error, text, count: 1 });
                lines.truncate(RECENT);
            }
        }
        drop(lines);
        self.changed.send_modify(|version| *version += 1);
    }
}

struct RecentLayer(Arc<Recent>);

impl<S: Subscriber> Layer<S> for RecentLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN {
            return;
        }
        let mut text = Text::default();
        event.record(&mut text);
        let at_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
        self.0.push(at_ms, level == Level::ERROR, text.finish());
    }
}

/// An event as one line: "message (field: value, …)".
#[derive(Default)]
struct Text {
    message: String,
    fields: Vec<String>,
}

impl Text {
    fn add(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push(format!("{}: {value}", field.name()));
        }
    }

    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else {
            format!("{} ({})", self.message, self.fields.join(", "))
        }
    }
}

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.add(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.add(field, format!("{value:?}"));
    }
}

/// Today's file in the folder, opened (and the folder made) when the first
/// line of the day is written. A file that can't be opened is tried again
/// the next day; the console still has the lines.
struct DailyFile {
    folder: PathBuf,
    open: Mutex<Option<(NaiveDate, Option<File>)>>,
}

impl DailyFile {
    fn new(folder: PathBuf) -> DailyFile {
        DailyFile { folder, open: Mutex::new(None) }
    }

    fn open(&self, day: NaiveDate) -> Option<File> {
        let path = self.folder.join(file_name(day));
        let file = std::fs::create_dir_all(&self.folder)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
        match file {
            Ok(file) => {
                prune(&self.folder, KEEP_DAYS);
                Some(file)
            }
            Err(error) => {
                // Not through tracing: this is inside it.
                eprintln!("couldn't open the log file {}: {error}", path.display());
                None
            }
        }
    }
}

impl<'a> MakeWriter<'a> for DailyFile {
    type Writer = &'a DailyFile;

    fn make_writer(&'a self) -> &'a DailyFile {
        self
    }
}

impl Write for &DailyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let today = Local::now().date_naive();
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if open.as_ref().is_none_or(|(day, _)| *day != today) {
            *open = Some((today, self.open(today)));
        }
        match open.as_mut().and_then(|(_, file)| file.as_mut()) {
            Some(file) => file.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        match open.as_mut().and_then(|(_, file)| file.as_mut()) {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

fn file_name(day: NaiveDate) -> String {
    format!("lsk.{day}.log")
}

/// The day a log file is for, if it's one of ours.
fn day_of(name: &str) -> Option<NaiveDate> {
    let day = name.strip_prefix("lsk.")?.strip_suffix(".log")?;
    NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
}

/// Deletes all but the newest `keep` days' files.
fn prune(folder: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut files: Vec<(NaiveDate, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| Some((day_of(entry.file_name().to_str()?)?, entry.path())))
        .collect();
    files.sort_by_key(|(day, _)| std::cmp::Reverse(*day));
    for (_, path) in files.into_iter().skip(keep) {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!("couldn't delete the old log file {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_named_by_day() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        assert_eq!(file_name(day), "lsk.2026-10-03.log");
        assert_eq!(day_of("lsk.2026-10-03.log"), Some(day));
        assert_eq!(day_of("lsk.2026-10-03.txt"), None);
        assert_eq!(day_of("notes.log"), None);
    }

    #[test]
    fn only_the_newest_days_are_kept() {
        let folder = std::env::temp_dir().join(format!("lsk-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        for day in 1..=9 {
            std::fs::write(folder.join(format!("lsk.2026-10-0{day}.log")), "").unwrap();
        }
        std::fs::write(folder.join("mine.txt"), "").unwrap();
        prune(&folder, 7);
        let mut left: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 8);
        assert_eq!(left[0], "lsk.2026-10-03.log");
        assert!(left.contains(&"mine.txt".to_string()));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn repeated_lines_are_counted() {
        let recent = Recent::default();
        recent.push(1, false, "a".into());
        recent.push(2, false, "a".into());
        recent.push(3, true, "b".into());
        for at in 0..30 {
            recent.push(at, false, format!("line {at}"));
        }
        let lines = recent.lines.lock().unwrap();
        assert_eq!(lines.len(), RECENT);
        assert_eq!(lines[0].text, "line 29");

        let recent = Recent::default();
        recent.push(1, false, "a".into());
        recent.push(2, false, "a".into());
        let lines = recent.lines.lock().unwrap();
        assert_eq!((lines[0].count, lines[0].at_ms), (2, 2));
    }

    #[test]
    fn events_read_as_one_line() {
        let recent = Arc::new(Recent::default());
        let subscriber = Registry::default().with(RecentLayer(Arc::clone(&recent)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("not kept");
            tracing::warn!(plugin = "foo", size = 3, "too big");
            tracing::error!("broke: {}", "why");
        });
        let lines = recent.lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].text.as_str(), lines[0].error), ("broke: why", true));
        assert_eq!((lines[1].text.as_str(), lines[1].error), ("too big (plugin: foo, size: 3)", false));
    }
}
