//! The chat log: every chat message the app sees, and what moderators did
//! to the viewers (in chat: a message deleted, a timeout, a ban; from the
//! panel: an unban, a warning, a VIP…), kept on disk for each viewer's
//! card in Activity.
//!
//! One folder per channel (`chat/twitch/<channel>/`), one file per day
//! (`2026-10-16.jsonl`), one JSON line per record, appended as chat comes
//! in. Days older than `chat_log.keep_days` are deleted. Only what the app
//! saw while it ran is in it; Twitch's own viewer card has the rest.
//!
//! A thread of its own writes, so a slow disk never holds up the pipeline.
//! It keeps an index in memory (each viewer's names, and where their lines
//! are in the files); a card reads its lines from the files.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use chrono::{Datelike, NaiveDate, TimeZone};
use protocol::chat::ChatBadge;
use protocol::viewers::{Done, FOUND_LIMIT, FoundViewer, LOG_PAGE, LineKind, LogLine};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::emote_text::EmoteText;
use crate::model::{EmoteRef, EmoteSpan, Input, InputKind, Moderation};
use crate::services::emotes::{EmoteInfo, EmoteRegistry};

/// The folder next to `lsk.json`.
pub const FOLDER: &str = "chat";

/// This many message ids are remembered, for chat's deletions (which come
/// right after the message).
const REMEMBERED: usize = 50_000;

/// What the log tells open cards.
#[derive(Debug, Clone, PartialEq)]
pub enum LogChange {
    /// A new line in a viewer's log.
    Line { viewer: String, line: LogLine },
    /// One of a viewer's messages was taken out of the chat.
    Deleted { viewer: String, message: String },
}

/// Who a line from the panel is about: a viewer the log may not have seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Someone {
    pub id: String,
    pub login: String,
    pub name: String,
}

/// A viewer's card as the log knows it (the panel adds what Twitch says).
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub login: String,
    pub name: String,
    pub color: Option<String>,
    pub badges: Vec<ChatBadge>,
    pub messages: usize,
    pub lines: Vec<LogLine>,
    pub older: usize,
}

#[derive(Clone)]
pub struct ChatLog {
    shared: Arc<Shared>,
    jobs: mpsc::Sender<Job>,
}

struct Shared {
    index: Mutex<Index>,
    changes: broadcast::Sender<LogChange>,
    emotes: Arc<EmoteRegistry>,
    root: PathBuf,
    keep_days: AtomicU32,
}

enum Job {
    Input(Box<Input>),
    Done { someone: Someone, done: Done, at_ms: u64 },
}

/// One line of a day's file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Record {
    Message {
        at: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        id: String,
        user: String,
        login: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        /// `set/version`, in chat's order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        badges: Vec<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        emotes: Vec<SavedEmote>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        action: bool,
    },
    Deleted {
        at: u64,
        id: String,
    },
    TimedOut {
        at: u64,
        user: String,
        secs: u64,
    },
    Banned {
        at: u64,
        user: String,
    },
    Done {
        at: u64,
        user: String,
        login: String,
        name: String,
        done: Done,
    },
}

impl Record {
    fn at(&self) -> u64 {
        match self {
            Record::Message { at, .. }
            | Record::Deleted { at, .. }
            | Record::TimedOut { at, .. }
            | Record::Banned { at, .. }
            | Record::Done { at, .. } => *at,
        }
    }
}

/// A platform's own emote in a message, with its image, so it shows after
/// a restart too (the image proxy only serves emotes it was told of).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedEmote {
    start: usize,
    end: usize,
    source: String,
    id: String,
    code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    url: String,
}

/// Where a line is: its day's file, and its bytes in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pos {
    day: i32,
    offset: u32,
    len: u32,
    message: bool,
    deleted: bool,
}

#[derive(Debug, Default)]
struct Viewer {
    login: String,
    name: String,
    color: Option<String>,
    last_seen_ms: u64,
    messages: usize,
    /// Oldest first, in the files' order.
    lines: Vec<Pos>,
}

#[derive(Default)]
struct Index {
    /// The folder its files are in (`twitch/<channel>`).
    folder: String,
    platform: String,
    channel_id: String,
    /// By user id.
    viewers: HashMap<String, Viewer>,
    /// Lowercase logins and names → user id.
    names: HashMap<String, String>,
    /// Recent messages by id → whose, and where.
    recent: HashMap<String, (String, i32, u32)>,
    recent_order: VecDeque<String>,
}

impl Index {
    /// Takes a record into the index; what changed, for open cards.
    fn apply(&mut self, record: &Record, pos: Pos) -> Option<Applied> {
        match record {
            Record::Message { at, id, user, login, name, color, .. } => {
                let viewer = self.viewers.entry(user.clone()).or_default();
                viewer.login = login.clone();
                viewer.name = name.clone();
                viewer.color = color.clone();
                viewer.last_seen_ms = viewer.last_seen_ms.max(*at);
                viewer.messages += 1;
                viewer.lines.push(Pos { message: true, ..pos });
                self.names.insert(login.to_lowercase(), user.clone());
                self.names.insert(name.to_lowercase(), user.clone());
                if !id.is_empty() {
                    self.remember(id, user, pos);
                }
                Some(Applied::Line(user.clone()))
            }
            Record::Deleted { id, .. } => {
                let (user, day, offset) = self.recent.get(id)?.clone();
                let viewer = self.viewers.get_mut(&user)?;
                let at =
                    viewer.lines.binary_search_by_key(&(day, offset), |pos| (pos.day, pos.offset)).ok()?;
                viewer.lines[at].deleted = true;
                Some(Applied::Deleted { viewer: user, message: id.clone() })
            }
            // Someone the log hasn't seen write has no name to show.
            Record::TimedOut { user, .. } | Record::Banned { user, .. } => {
                self.viewers.get_mut(user)?.lines.push(pos);
                Some(Applied::Line(user.clone()))
            }
            Record::Done { user, login, name, .. } => {
                let viewer = self.viewers.entry(user.clone()).or_default();
                if viewer.login.is_empty() {
                    viewer.login = login.clone();
                    viewer.name = name.clone();
                    self.names.insert(login.to_lowercase(), user.clone());
                    self.names.insert(name.to_lowercase(), user.clone());
                }
                viewer.lines.push(pos);
                Some(Applied::Line(user.clone()))
            }
        }
    }

    fn remember(&mut self, id: &str, user: &str, pos: Pos) {
        self.recent.insert(id.to_string(), (user.to_string(), pos.day, pos.offset));
        self.recent_order.push_back(id.to_string());
        while self.recent_order.len() > REMEMBERED {
            if let Some(old) = self.recent_order.pop_front() {
                self.recent.remove(&old);
            }
        }
    }

    /// Forgets the days before `first`.
    fn prune(&mut self, first: i32) {
        let mut gone = false;
        for viewer in self.viewers.values_mut() {
            if viewer.lines.first().is_some_and(|pos| pos.day < first) {
                viewer.lines.retain(|pos| pos.day >= first);
                viewer.messages = viewer.lines.iter().filter(|pos| pos.message).count();
                gone = true;
            }
        }
        if gone {
            self.viewers.retain(|_, viewer| !viewer.lines.is_empty());
            let viewers = &self.viewers;
            self.names.retain(|_, id| viewers.contains_key(id));
        }
    }
}

enum Applied {
    Line(String),
    Deleted { viewer: String, message: String },
}

impl ChatLog {
    /// Opens the log in `dir` (next to `lsk.json`) for chat's `channel`,
    /// and starts its thread. `writes` off (replaying a recording), nothing
    /// is written.
    pub fn open(
        dir: &Path,
        channel: &str,
        keep_days: u32,
        emotes: Arc<EmoteRegistry>,
        writes: bool,
    ) -> ChatLog {
        let shared = Arc::new(Shared {
            index: Mutex::new(Index::default()),
            changes: broadcast::channel(256).0,
            emotes,
            root: dir.join(FOLDER),
            keep_days: AtomicU32::new(keep_days.max(1)),
        });
        let (jobs, receiver) = mpsc::channel();
        let writer = Writer { shared: Arc::clone(&shared), file: None, writes, failed: false };
        let channel = channel.to_string();
        let started =
            std::thread::Builder::new().name("chat log".into()).spawn(move || writer.run(&channel, receiver));
        if let Err(error) = started {
            tracing::error!("couldn't start the chat log: {error}");
        }
        ChatLog { shared, jobs }
    }

    /// A log that keeps nothing (for tests elsewhere).
    #[cfg(test)]
    pub fn off() -> ChatLog {
        let shared = Arc::new(Shared {
            index: Mutex::new(Index::default()),
            changes: broadcast::channel(1).0,
            emotes: Arc::new(EmoteRegistry::for_tests()),
            root: PathBuf::new(),
            keep_days: AtomicU32::new(1),
        });
        ChatLog { shared, jobs: mpsc::channel().0 }
    }

    /// A chat message or a moderator's action, from the pipeline. Test
    /// events aren't chat.
    pub fn observe(&self, input: &Input) {
        if input.origin.provider == crate::pipeline::TEST_PROVIDER
            || !matches!(
                input.kind,
                InputKind::Message(_) | InputKind::Moderation(_) | InputKind::ChannelJoined
            )
        {
            return;
        }
        let _ = self.jobs.send(Job::Input(Box::new(input.clone())));
    }

    /// Something done from the panel that chat doesn't say.
    pub fn note(&self, someone: Someone, done: Done) {
        let at_ms = crate::pipeline::now_ms();
        let _ = self.jobs.send(Job::Done { someone, done, at_ms });
    }

    pub fn set_keep_days(&self, days: u32) {
        self.shared.keep_days.store(days.max(1), Ordering::Relaxed);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogChange> {
        self.shared.changes.subscribe()
    }

    /// The viewers with `text` in their login or name: those it starts
    /// first, then the most recently seen.
    pub fn find(&self, text: &str) -> Vec<FoundViewer> {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            return Vec::new();
        }
        let index = self.shared.index.lock().unwrap();
        let mut found: Vec<(bool, u64, &String, &Viewer)> = index
            .viewers
            .iter()
            .filter(|(_, viewer)| !viewer.login.is_empty())
            .filter_map(|(id, viewer)| {
                let (login, name) = (viewer.login.to_lowercase(), viewer.name.to_lowercase());
                let starts = login.starts_with(&text) || name.starts_with(&text);
                (starts || login.contains(&text) || name.contains(&text)).then_some((
                    starts,
                    viewer.last_seen_ms,
                    id,
                    viewer,
                ))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2)));
        found
            .into_iter()
            .take(FOUND_LIMIT)
            .map(|(_, last_seen_ms, id, viewer)| FoundViewer {
                id: id.clone(),
                name: viewer.name.clone(),
                login: viewer.login.clone(),
                color: viewer.color.clone(),
                last_seen_ms,
            })
            .collect()
    }

    /// Viewer `id`'s login and name, if the log has seen them.
    pub fn someone(&self, id: &str) -> Option<Someone> {
        let index = self.shared.index.lock().unwrap();
        let viewer = index.viewers.get(id).filter(|viewer| !viewer.login.is_empty())?;
        Some(Someone { id: id.to_string(), login: viewer.login.clone(), name: viewer.name.clone() })
    }

    /// The id of the viewer with this login or name, if the log has seen
    /// them.
    pub fn id_of(&self, name: &str) -> Option<String> {
        self.shared.index.lock().unwrap().names.get(&name.trim().to_lowercase()).cloned()
    }

    /// Viewer `id`'s card: their newest lines, read from the files.
    pub fn card(&self, id: &str) -> Option<Card> {
        let (folder, platform, channel_id, viewer, positions, older, last) = {
            let index = self.shared.index.lock().unwrap();
            let viewer = index.viewers.get(id)?;
            let older = viewer.lines.len().saturating_sub(LOG_PAGE);
            let last = viewer.lines.iter().rev().find(|pos| pos.message).copied();
            (
                index.folder.clone(),
                index.platform.clone(),
                index.channel_id.clone(),
                (viewer.login.clone(), viewer.name.clone(), viewer.color.clone(), viewer.messages),
                viewer.lines[older..].to_vec(),
                older,
                last,
            )
        };
        let folder = self.shared.root.join(folder);
        let badges = match last.and_then(|pos| read(&folder, &[pos]).pop().flatten()) {
            Some(Record::Message { badges, .. }) => badges
                .iter()
                .filter_map(|badge| badge.split_once('/'))
                .map(|(set, version)| match self.shared.emotes.badge(&platform, &channel_id, set, version) {
                    Some(known) => ChatBadge {
                        set: set.to_string(),
                        title: known.title,
                        image: Some(known.image.path()),
                    },
                    None => ChatBadge { set: set.to_string(), title: set.to_string(), image: None },
                })
                .collect(),
            _ => Vec::new(),
        };
        let lines = self.lines_at(&folder, &platform, &channel_id, &positions);
        let (login, name, color, messages) = viewer;
        Some(Card { id: id.to_string(), login, name, color, badges, messages, lines, older })
    }

    /// The page of viewer `id`'s lines before line `before`, and how many
    /// come before it.
    pub fn older(&self, id: &str, before: usize) -> Option<(Vec<LogLine>, usize)> {
        let (folder, platform, channel_id, positions, first) = {
            let index = self.shared.index.lock().unwrap();
            let viewer = index.viewers.get(id)?;
            let before = before.min(viewer.lines.len());
            let first = before.saturating_sub(LOG_PAGE);
            (
                index.folder.clone(),
                index.platform.clone(),
                index.channel_id.clone(),
                viewer.lines[first..before].to_vec(),
                first,
            )
        };
        let folder = self.shared.root.join(folder);
        Some((self.lines_at(&folder, &platform, &channel_id, &positions), first))
    }

    fn lines_at(&self, folder: &Path, platform: &str, channel_id: &str, positions: &[Pos]) -> Vec<LogLine> {
        read(folder, positions)
            .into_iter()
            .zip(positions)
            .filter_map(|(record, pos)| {
                line(&record?, pos.deleted, platform, channel_id, &self.shared.emotes)
            })
            .collect()
    }
}

/// The records at `positions`, read from their days' files; `None` for one
/// that can't be read.
fn read(folder: &Path, positions: &[Pos]) -> Vec<Option<Record>> {
    let mut open: Option<(i32, File)> = None;
    positions
        .iter()
        .map(|pos| {
            if open.as_ref().is_none_or(|(day, _)| *day != pos.day) {
                open = File::open(folder.join(file_name(pos.day))).ok().map(|file| (pos.day, file));
            }
            let (_, file) = open.as_mut()?;
            let mut bytes = vec![0; pos.len as usize];
            file.seek(SeekFrom::Start(u64::from(pos.offset))).ok()?;
            file.read_exact(&mut bytes).ok()?;
            serde_json::from_slice(&bytes).ok()
        })
        .collect()
}

/// How a record reads on a card.
fn line(
    record: &Record,
    deleted: bool,
    platform: &str,
    channel_id: &str,
    emotes: &EmoteRegistry,
) -> Option<LogLine> {
    let kind = match record {
        Record::Message { id, text, emotes: saved, action, .. } => {
            let spans: Vec<EmoteSpan> = saved
                .iter()
                .map(|emote| EmoteSpan {
                    start: emote.start,
                    end: emote.end,
                    emote: EmoteRef {
                        source: emote.source.clone(),
                        id: emote.id.clone(),
                        code: emote.code.clone(),
                    },
                })
                .collect();
            for (span, emote) in spans.iter().zip(saved) {
                if !emote.url.is_empty() {
                    let info = EmoteInfo {
                        emote: span.emote.clone(),
                        image_url: emote.url.clone(),
                        zero_width: false,
                    };
                    emotes.register_native(platform, channel_id, info);
                }
            }
            let parts =
                EmoteText::from_message(text, &spans, |code| emotes.lookup_code(platform, channel_id, code))
                    .parts();
            LineKind::Message { id: id.clone(), text: text.clone(), parts, deleted, action: *action }
        }
        Record::Deleted { .. } => return None,
        Record::TimedOut { secs, .. } => LineKind::TimedOut { secs: *secs },
        Record::Banned { .. } => LineKind::Banned,
        Record::Done { done, .. } => LineKind::Done { done: *done },
    };
    Some(LogLine { at_ms: record.at(), kind })
}

/// The day a time falls on, here (days since 1 Jan of year 1).
fn day_of(at_ms: u64) -> i32 {
    let at = chrono::Local.timestamp_millis_opt(at_ms as i64).single();
    at.map_or(0, |at| at.date_naive().num_days_from_ce())
}

/// A day's file: `2026-10-16.jsonl`.
fn file_name(day: i32) -> String {
    let date = NaiveDate::from_num_days_from_ce_opt(day).unwrap_or_default();
    format!("{}.jsonl", date.format("%Y-%m-%d"))
}

/// The day a file is for, from its name.
fn day_of_file(name: &str) -> Option<i32> {
    let date = name.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok().map(|date| date.num_days_from_ce())
}

/// The channel's folder in `chat/`: `twitch/vedal987`.
fn folder_of(platform: &str, channel: &str) -> String {
    let clean = |name: &str| -> String {
        name.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
    };
    format!("{}/{}", clean(platform), clean(channel))
}

/// The thread that writes the files and keeps the index.
struct Writer {
    shared: Arc<Shared>,
    /// Today's file: its day, and its length.
    file: Option<(i32, File, u64)>,
    writes: bool,
    /// A write failed: said once.
    failed: bool,
}

impl Writer {
    fn run(mut self, channel: &str, jobs: mpsc::Receiver<Job>) {
        if !channel.is_empty() {
            self.load("twitch", channel);
        }
        while let Ok(job) = jobs.recv() {
            self.handle(job);
        }
    }

    fn handle(&mut self, job: Job) {
        let record = match job {
            Job::Input(input) => {
                let (platform, channel) = (&input.origin.platform, &input.origin.channel);
                let folder = folder_of(platform, &channel.name);
                if self.shared.index.lock().unwrap().folder != folder {
                    self.load(platform, &channel.name);
                }
                self.shared.index.lock().unwrap().channel_id = channel.id.clone();
                match record_of(&input, &self.shared.emotes) {
                    Some(record) => record,
                    None => return,
                }
            }
            Job::Done { someone, done, at_ms } => {
                Record::Done { at: at_ms, user: someone.id, login: someone.login, name: someone.name, done }
            }
        };
        let Some(pos) = self.append(&record) else { return };
        let mut index = self.shared.index.lock().unwrap();
        let change = match index.apply(&record, pos) {
            Some(Applied::Line(viewer)) => {
                let (platform, channel_id) = (index.platform.clone(), index.channel_id.clone());
                drop(index);
                line(&record, false, &platform, &channel_id, &self.shared.emotes)
                    .map(|line| LogChange::Line { viewer, line })
            }
            Some(Applied::Deleted { viewer, message }) => Some(LogChange::Deleted { viewer, message }),
            None => None,
        };
        if let Some(change) = change {
            let _ = self.shared.changes.send(change);
        }
    }

    /// Writes a record at the end of its day's file; where it went.
    fn append(&mut self, record: &Record) -> Option<Pos> {
        let mut bytes = serde_json::to_vec(record).ok()?;
        bytes.push(b'\n');
        let day = day_of(record.at());
        let folder = self.folder();
        if !self.writes {
            return None;
        }
        if self.file.as_ref().is_none_or(|(open, _, _)| *open != day) {
            self.prune(day);
            let opened = fs::create_dir_all(&folder)
                .and_then(|()| OpenOptions::new().create(true).append(true).open(folder.join(file_name(day))))
                .and_then(|file| Ok((file.metadata()?.len(), file)));
            match opened {
                Ok((len, file)) => self.file = Some((day, file, len)),
                Err(error) => {
                    self.failed(&folder, &error);
                    return None;
                }
            }
        }
        let (_, file, len) = self.file.as_mut()?;
        let offset = u32::try_from(*len).ok()?;
        if let Err(error) = file.write_all(&bytes) {
            self.failed(&folder, &error);
            return None;
        }
        *len += bytes.len() as u64;
        Some(Pos { day, offset, len: bytes.len() as u32, message: false, deleted: false })
    }

    fn failed(&mut self, folder: &Path, error: &std::io::Error) {
        if !self.failed {
            tracing::warn!("couldn't write the chat log in {}: {error}", folder.display());
            self.failed = true;
        }
    }

    fn folder(&self) -> PathBuf {
        self.shared.root.join(&self.shared.index.lock().unwrap().folder)
    }

    /// The first day kept, when `today` is today.
    fn first_kept(&self, today: i32) -> i32 {
        today - self.shared.keep_days.load(Ordering::Relaxed) as i32 + 1
    }

    /// Deletes the days that are too old, on disk and in the index.
    fn prune(&mut self, today: i32) {
        let first = self.first_kept(today);
        self.shared.index.lock().unwrap().prune(first);
        let folder = self.folder();
        let Ok(files) = fs::read_dir(&folder) else { return };
        for file in files.flatten() {
            let name = file.file_name();
            if day_of_file(&name.to_string_lossy()).is_some_and(|day| day < first)
                && let Err(error) = fs::remove_file(file.path())
            {
                tracing::warn!("couldn't delete {}: {error}", file.path().display());
            }
        }
    }

    /// Reads the channel's kept days into a new index.
    fn load(&mut self, platform: &str, channel: &str) {
        let folder_name = folder_of(platform, channel);
        let folder = self.shared.root.join(&folder_name);
        self.file = None;
        let first = self.first_kept(day_of(crate::pipeline::now_ms()));
        let mut days: Vec<(i32, PathBuf)> = fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|file| Some((day_of_file(&file.file_name().to_string_lossy())?, file.path())))
            .filter(|(day, _)| *day >= first)
            .collect();
        days.sort();
        let mut index = Index { folder: folder_name, platform: platform.to_string(), ..Index::default() };
        let (mut lines, mut unreadable) = (0usize, 0usize);
        for (day, path) in days {
            let Ok(file) = File::open(&path) else { continue };
            let mut reader = BufReader::new(file);
            let (mut offset, mut text) = (0u64, String::new());
            loop {
                text.clear();
                let Ok(read) = reader.read_line(&mut text) else { break };
                if read == 0 {
                    break;
                }
                let Ok(at) = u32::try_from(offset) else { break };
                let pos = Pos { day, offset: at, len: read as u32, message: false, deleted: false };
                offset += read as u64;
                match serde_json::from_str::<Record>(text.trim_end()) {
                    Ok(record) => {
                        index.apply(&record, pos);
                        lines += 1;
                    }
                    // A line cut short by a crash, or written by a newer version.
                    Err(_) => unreadable += 1,
                }
            }
        }
        if unreadable > 0 {
            tracing::warn!(unreadable, "some chat log lines couldn't be read; they're left out");
        }
        tracing::info!(channel, lines, viewers = index.viewers.len(), "read the chat log");
        *self.shared.index.lock().unwrap() = index;
        if self.writes {
            self.prune(day_of(crate::pipeline::now_ms()));
        }
    }
}

/// What an input puts in the log.
fn record_of(input: &Input, emotes: &EmoteRegistry) -> Option<Record> {
    let at = input.at_ms;
    Some(match &input.kind {
        InputKind::Message(message) => {
            let user = &message.user;
            let saved = message
                .emotes
                .iter()
                .map(|span| SavedEmote {
                    start: span.start,
                    end: span.end,
                    source: span.emote.source.clone(),
                    id: span.emote.id.clone(),
                    code: span.emote.code.clone(),
                    url: emotes.image_url(&span.emote.source, &span.emote.id).unwrap_or_default(),
                })
                .collect();
            Record::Message {
                at,
                id: message.id.clone(),
                user: user.id.clone(),
                login: user.name.clone(),
                name: if user.display_name.is_empty() {
                    user.name.clone()
                } else {
                    user.display_name.clone()
                },
                color: user.color.clone(),
                badges: user.badges.iter().map(|badge| format!("{}/{}", badge.set, badge.version)).collect(),
                text: message.text.clone(),
                emotes: saved,
                action: message.marks.action,
            }
        }
        InputKind::Moderation(Moderation::MessageDeleted { id }) => Record::Deleted { at, id: id.clone() },
        InputKind::Moderation(Moderation::UserCleared { user_id, timeout_secs: Some(secs) }) => {
            Record::TimedOut { at, user: user_id.clone(), secs: *secs }
        }
        InputKind::Moderation(Moderation::UserCleared { user_id, timeout_secs: None }) => {
            Record::Banned { at, user: user_id.clone() }
        }
        InputKind::Moderation(Moderation::ChatCleared) | InputKind::ChannelJoined | InputKind::Event(_) => {
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Badge, ChannelRef, Message, Origin, User};

    fn input(at_ms: u64, kind: InputKind) -> Input {
        Input {
            at_ms,
            origin: Origin {
                provider: "twitch_chat".into(),
                platform: "twitch".into(),
                channel: ChannelRef { id: "1".into(), name: "Chan".into() },
            },
            kind,
        }
    }

    fn message(at_ms: u64, id: &str, login: &str, text: &str) -> Input {
        let user = User {
            id: format!("id-{login}"),
            name: login.into(),
            display_name: login.to_uppercase(),
            color: Some("#5fb8ff".into()),
            badges: vec![Badge { set: "subscriber".into(), version: "2".into() }],
            ..Default::default()
        };
        input(
            at_ms,
            InputKind::Message(Message { id: id.into(), user, text: text.into(), ..Default::default() }),
        )
    }

    /// A folder of its own for each test, emptied first.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsk_chat_log_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn log(dir: &Path) -> ChatLog {
        let emotes = Arc::new(EmoteRegistry::for_tests());
        ChatLog::open(dir, "chan", 90, emotes, true)
    }

    /// Waits until the thread has taken in everything sent so far.
    fn settle(log: &ChatLog, viewer: &str, lines: usize) {
        for _ in 0..200 {
            let count =
                log.shared.index.lock().unwrap().viewers.get(viewer).map_or(0, |viewer| viewer.lines.len());
            if count >= lines {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the chat log didn't take in {lines} lines for {viewer}");
    }

    const DAY: u64 = 86_400_000;

    #[test]
    fn writes_reads_and_finds() {
        let dir = scratch("writes");
        let now = crate::pipeline::now_ms();
        let chat = log(&dir);
        chat.observe(&message(now - 2 * DAY, "m1", "mossy", "hi chat"));
        chat.observe(&message(now - DAY, "m2", "mossy", "spoiler"));
        chat.observe(&input(
            now - DAY + 5,
            InputKind::Moderation(Moderation::MessageDeleted { id: "m2".into() }),
        ));
        chat.observe(&input(
            now - DAY + 6,
            InputKind::Moderation(Moderation::UserCleared {
                user_id: "id-mossy".into(),
                timeout_secs: Some(600),
            }),
        ));
        chat.observe(&message(now, "m3", "other_mossman", "hello"));
        chat.note(
            Someone { id: "id-mossy".into(), login: "mossy".into(), name: "MOSSY".into() },
            Done::VipAdded,
        );
        settle(&chat, "id-mossy", 4);

        let card = chat.card("id-mossy").unwrap();
        assert_eq!((card.name.as_str(), card.messages, card.older), ("MOSSY", 2, 0));
        assert_eq!(card.badges[0].set, "subscriber");
        let kinds: Vec<&LineKind> = card.lines.iter().map(|line| &line.kind).collect();
        assert!(matches!(kinds[0], LineKind::Message { text, deleted: false, .. } if text == "hi chat"));
        assert!(matches!(kinds[1], LineKind::Message { text, deleted: true, .. } if text == "spoiler"));
        assert_eq!(kinds[2], &LineKind::TimedOut { secs: 600 });
        assert_eq!(kinds[3], &LineKind::Done { done: Done::VipAdded });

        let found = chat.find("MOSS");
        let names: Vec<&str> = found.iter().map(|viewer| viewer.login.as_str()).collect();
        assert_eq!(names, ["mossy", "other_mossman"], "those it starts first");
        assert_eq!(chat.id_of("Mossy").as_deref(), Some("id-mossy"));
        assert!(chat.find("  ").is_empty());

        // Read again from the files, as after a restart.
        drop(chat);
        let again = log(&dir);
        settle(&again, "id-mossy", 4);
        assert_eq!(again.card("id-mossy").unwrap().lines, card.lines);
        let days = fs::read_dir(dir.join("chat/twitch/chan")).unwrap().count();
        assert_eq!(days, 3, "one file a day");
    }

    #[test]
    fn older_lines_a_page_at_a_time() {
        let dir = scratch("older");
        let now = crate::pipeline::now_ms();
        let chat = log(&dir);
        let count = LOG_PAGE + 7;
        for n in 0..count {
            chat.observe(&message(now + n as u64, &format!("m{n}"), "busy", &format!("line {n}")));
        }
        settle(&chat, "id-busy", count);
        let card = chat.card("id-busy").unwrap();
        assert_eq!((card.lines.len(), card.older), (LOG_PAGE, 7));
        let (older, first) = chat.older("id-busy", card.older).unwrap();
        assert_eq!((older.len(), first), (7, 0));
        assert!(matches!(&older[0].kind, LineKind::Message { text, .. } if text == "line 0"));
    }

    #[test]
    fn old_days_go() {
        let dir = scratch("old_days");
        let now = crate::pipeline::now_ms();
        let chat = log(&dir);
        chat.set_keep_days(2);
        chat.observe(&message(now - 5 * DAY, "m1", "old", "long ago"));
        chat.observe(&message(now, "m2", "new", "today"));
        settle(&chat, "id-new", 1);
        assert!(chat.card("id-old").is_none(), "pruned when today's file opened");
        assert_eq!(fs::read_dir(dir.join("chat/twitch/chan")).unwrap().count(), 1);
    }

    #[test]
    fn test_events_are_not_chat() {
        let dir = scratch("tests");
        let chat = log(&dir);
        let mut test = message(crate::pipeline::now_ms(), "t1", "tester", "hi");
        test.origin.provider = crate::pipeline::TEST_PROVIDER.into();
        chat.observe(&test);
        chat.observe(&message(crate::pipeline::now_ms(), "m1", "real", "hi"));
        settle(&chat, "id-real", 1);
        assert!(chat.card("id-tester").is_none());
    }

    #[test]
    fn day_files() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap().num_days_from_ce();
        assert_eq!(file_name(day), "2026-10-16.jsonl");
        assert_eq!(day_of_file("2026-10-16.jsonl"), Some(day));
        assert_eq!(day_of_file("notes.txt"), None);
        assert_eq!(folder_of("twitch", "Vedal987"), "twitch/vedal987");
        assert_eq!(folder_of("twitch", "../x"), "twitch/x");
    }
}
