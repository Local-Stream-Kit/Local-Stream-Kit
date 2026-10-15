//! Text to speech (see `protocol::speech`): a worker thread makes spoken
//! messages one at a time, in the order they're asked for. Each becomes a
//! WAV file in the speech cache; whoever asked is told its name. When it
//! plays is up to them: the queue (`services::activity`) makes a waiting
//! message shortly before its turn. Kokoro loads on the first message.
//!
//! A message in a plugin's voice (`<plugin>:<voice>`) is made by that
//! plugin instead: its server code or program, while it runs, is a
//! [`Speaker`]. Its audio (WAV, MP3 or Ogg) goes into the same cache. If
//! the plugin isn't running, the message is read in the app's voice.

pub mod install;
pub mod kokoro;
mod onnx;
mod phonemizer;
pub mod text;

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use protocol::plugin::split_voice;
use protocol::speech::{SpeechStatus, speech_path};
use protocol::{SpeechClip, SpeechSettings};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use self::kokoro::{Files, Kokoro, SAMPLE_RATE};
use crate::services::status::StatusHub;
use crate::web::AppState;

/// Spoken messages kept in the cache (a repeated message isn't made again).
const KEEP_FILES: usize = 50;
/// How long a plugin has to make a message.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(30);
/// The biggest message a plugin may give.
pub const MAX_AUDIO: usize = 8 * 1024 * 1024;
/// The audio formats a plugin's message may be in, as file extensions.
const FORMATS: [&str; 3] = ["wav", "mp3", "ogg"];

/// One message to read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub text: String,
    pub voice: String,
    pub speed: f32,
    /// 0–1, the speech volume (with the node's voice change); the page
    /// multiplies it by the master volume, as for every sound.
    pub volume: f32,
}

/// A message made: its file in the speech cache, and its length if known
/// (WAV files).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Made {
    pub file: String,
    #[serde(default)]
    pub duration_ms: Option<u32>,
}

/// A message a plugin made: a whole audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub data: Vec<u8>,
    /// `wav`, `mp3` or `ogg`.
    pub format: String,
}

/// The app asking a plugin for a message in one of its voices (its own
/// id, without the plugin's).
#[derive(Debug)]
pub struct SpeakRequest {
    pub voice: String,
    pub text: String,
    pub speed: f32,
    pub answer: oneshot::Sender<Result<Audio, String>>,
}

/// A running plugin with voices: where its messages are asked for.
pub type Speaker = mpsc::UnboundedSender<SpeakRequest>;

impl Made {
    /// For the overlay, at a volume (0–1).
    pub fn clip(&self, volume: f32) -> SpeechClip {
        SpeechClip { url: speech_path(&self.file), duration_ms: self.duration_ms, volume }
    }
}

/// Told the message's file once it's made, or `None` if it couldn't be.
pub type OnMade = Box<dyn FnOnce(Option<Made>) + Send>;

/// An [`OnMade`] that's told exactly once: dropped untold (a panic on the
/// way, a worker that stopped), it's told `None`, so whoever waits for the
/// message (the queue) never waits forever.
struct Told(Option<OnMade>);

impl Told {
    fn new(on_made: OnMade) -> Told {
        Told(Some(on_made))
    }

    fn tell(mut self, made: Option<Made>) {
        if let Some(on_made) = self.0.take() {
            on_made(made);
        }
    }
}

impl Drop for Told {
    fn drop(&mut self) {
        if let Some(on_made) = self.0.take() {
            on_made(None);
        }
    }
}

/// Text to speech. Cheap to clone.
#[derive(Clone)]
pub struct Speech {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    /// The model, voices and ONNX Runtime.
    dir: PathBuf,
    /// Spoken messages.
    out: PathBuf,
    status: StatusHub,
    http: reqwest::Client,
    /// Asked for and not yet started.
    queue: Mutex<VecDeque<(Job, Told)>>,
    wake: Condvar,
    installing: AtomicBool,
    /// Plugins with voices that are running, by id.
    speakers: Mutex<HashMap<String, Speaker>>,
    /// Where plugins' answers are waited for (none in tests).
    runtime: Option<tokio::runtime::Handle>,
}

impl Speech {
    pub fn start(dir: PathBuf, out: PathBuf, status: StatusHub, http: reqwest::Client) -> Speech {
        let ready = install::installed(&Files::in_dir(&dir));
        status.set_speech(if ready { SpeechStatus::Ready } else { SpeechStatus::NotInstalled });
        let inner = Arc::new(Inner {
            dir,
            out,
            status,
            http,
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            installing: AtomicBool::new(false),
            speakers: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::try_current().ok(),
        });
        let worker = Arc::clone(&inner);
        let started = std::thread::Builder::new().name("speech".into()).spawn(move || worker.run());
        if let Err(error) = started {
            tracing::error!(%error, "couldn't start text to speech");
        }
        Speech { inner: Some(inner) }
    }

    /// Text to speech that never speaks (tests).
    #[cfg(test)]
    pub fn off() -> Speech {
        Speech { inner: None }
    }

    /// Text to speech that's ready and takes messages, with no worker to
    /// make them (tests; see [`Speech::queued`] and [`Speech::finish`]).
    #[cfg(test)]
    pub fn queue_only() -> Speech {
        let status = StatusHub::new();
        status.set_speech(SpeechStatus::Ready);
        Speech {
            inner: Some(Arc::new(Inner {
                dir: PathBuf::new(),
                out: PathBuf::new(),
                status,
                http: reqwest::Client::new(),
                queue: Mutex::new(VecDeque::new()),
                wake: Condvar::new(),
                installing: AtomicBool::new(false),
                speakers: Mutex::new(HashMap::new()),
                runtime: None,
            })),
        }
    }

    /// The messages asked for and not made yet (tests).
    #[cfg(test)]
    pub fn queued(&self) -> Vec<Job> {
        self.inner
            .as_ref()
            .map(|inner| inner.queue.lock().unwrap().iter().map(|(job, _)| job.clone()).collect())
            .unwrap_or_default()
    }

    /// Answers every message asked for, as the worker would (tests).
    #[cfg(test)]
    pub fn finish(&self, made: impl Fn(&Job) -> Option<Made>) {
        let Some(inner) = &self.inner else { return };
        let jobs: Vec<(Job, Told)> = inner.queue.lock().unwrap().drain(..).collect();
        for (job, told) in jobs {
            told.tell(made(&job));
        }
    }

    /// Whether the app's own voices can read (installed and not failed).
    pub fn ready(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.status.speech() == SpeechStatus::Ready)
    }

    /// Whether a message in `voice` can be read: by its plugin while it
    /// runs, else (and for the app's voices) by the app.
    pub fn can_speak(&self, voice: &str) -> bool {
        self.speaker(voice).is_some() || self.ready()
    }

    /// Makes a running plugin with voices the one asked for messages in
    /// them.
    pub fn add_speaker(&self, plugin: &str, speaker: Speaker) {
        if let Some(inner) = &self.inner
            && let Ok(mut speakers) = inner.speakers.lock()
        {
            speakers.insert(plugin.to_string(), speaker);
        }
    }

    /// A plugin stopped (or disconnected): unless it was started again
    /// since (a new `speaker`), the app reads its voices' messages.
    pub fn remove_speaker(&self, plugin: &str, speaker: &Speaker) {
        if let Some(inner) = &self.inner
            && let Ok(mut speakers) = inner.speakers.lock()
            && speakers.get(plugin).is_some_and(|known| known.same_channel(speaker))
        {
            speakers.remove(plugin);
        }
    }

    /// The running plugin that makes messages in `voice`, and the voice's
    /// own id.
    fn speaker<'v>(&self, voice: &'v str) -> Option<(Speaker, &'v str)> {
        let (plugin, own) = split_voice(voice)?;
        let inner = self.inner.as_ref()?;
        inner.runtime.as_ref()?;
        let speaker = inner.speakers.lock().ok()?.get(plugin).cloned()?;
        Some((speaker, own))
    }

    /// Asks for a message to be made, after those asked for before it;
    /// `on_made` is told its file (on the worker's thread, or a task for a
    /// plugin's voice). `false`, and `on_made` is dropped, if it can't be
    /// (empty, or not installed).
    pub fn make(&self, mut job: Job, on_made: impl FnOnce(Option<Made>) + Send + 'static) -> bool {
        let Some(inner) = &self.inner else { return false };
        if job.text.trim().is_empty() {
            return false;
        }
        if let Some((speaker, voice)) = self.speaker(&job.voice) {
            let voice = voice.to_string();
            return inner.ask(speaker, voice, job, Box::new(on_made));
        }
        if split_voice(&job.voice).is_some() {
            tracing::info!(voice = job.voice, "the voice's plugin isn't running; read in the app's voice");
            job.voice = SpeechSettings::default().voice;
        }
        if inner.status.speech() != SpeechStatus::Ready {
            tracing::info!("text to speech isn't installed yet (Plugins → Speak); a message wasn't read");
            return false;
        }
        let Ok(mut queue) = inner.queue.lock() else { return false };
        queue.push_back((job, Told::new(Box::new(on_made))));
        inner.wake.notify_one();
        true
    }

    /// Whether a message made earlier is still in the cache (which keeps
    /// the newest few). Always, in tests without a cache.
    pub fn has(&self, made: &Made) -> bool {
        match self.out_dir() {
            Some(dir) if !dir.as_os_str().is_empty() => {
                valid_name(&made.file) && dir.join(&made.file).is_file()
            }
            _ => true,
        }
    }

    /// Downloads what's missing (see `install.rs`), reporting progress in the
    /// status. Does nothing while a download is running.
    pub async fn install(&self) {
        let Some(inner) = &self.inner else { return };
        if inner.installing.swap(true, Ordering::SeqCst) {
            return;
        }
        let status = match install::install(&inner.http, &inner.dir, &inner.status).await {
            Ok(()) => {
                tracing::info!("text to speech is installed");
                SpeechStatus::Ready
            }
            Err(error) => {
                tracing::warn!("couldn't install text to speech: {error:#}");
                SpeechStatus::Failed { error: format!("{error:#}") }
            }
        };
        inner.status.set_speech(status);
        inner.installing.store(false, Ordering::SeqCst);
    }

    fn out_dir(&self) -> Option<&Path> {
        self.inner.as_ref().map(|inner| inner.out.as_path())
    }
}

impl Inner {
    /// Asks a plugin for a message, and waits for it on a task.
    fn ask(self: &Arc<Self>, speaker: Speaker, voice: String, job: Job, on_made: OnMade) -> bool {
        let Some(runtime) = &self.runtime else { return false };
        let (answer, answered) = oneshot::channel();
        let request = SpeakRequest { voice, text: job.text.clone(), speed: job.speed, answer };
        if speaker.send(request).is_err() {
            return false;
        }
        let inner = Arc::clone(self);
        let told = Told::new(on_made);
        runtime.spawn(async move {
            let audio = match tokio::time::timeout(SPEAK_TIMEOUT, answered).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err("the plugin stopped".to_string()),
                Err(_) => Err(format!("it took longer than {} seconds", SPEAK_TIMEOUT.as_secs())),
            };
            let made = audio.and_then(|audio| inner.keep(&job, &audio));
            match made {
                Ok(made) => told.tell(Some(made)),
                Err(error) => {
                    tracing::warn!(voice = job.voice, "couldn't read a message aloud: {error}");
                    told.tell(None);
                }
            }
        });
        true
    }

    /// A plugin's message, saved in the cache.
    fn keep(&self, job: &Job, audio: &Audio) -> Result<Made, String> {
        let format = audio.format.to_ascii_lowercase();
        if !FORMATS.contains(&format.as_str()) {
            return Err(format!("{:?} isn't wav, mp3 or ogg", audio.format));
        }
        if audio.data.is_empty() || audio.data.len() > MAX_AUDIO {
            return Err(format!("the audio is empty or bigger than {} MB", MAX_AUDIO / 1024 / 1024));
        }
        let file = format!("{:016x}.{format}", job_hash(job));
        let written = std::fs::create_dir_all(&self.out)
            .and_then(|()| std::fs::write(self.out.join(&file), &audio.data));
        written.map_err(|error| format!("couldn't save it: {error}"))?;
        prune(&self.out);
        let duration_ms = if format == "wav" { wav_duration_ms(&audio.data) } else { None };
        Ok(Made { file, duration_ms })
    }

    fn run(&self) {
        let mut engine: Option<Kokoro> = None;
        while let Some((job, told)) = self.next_job() {
            self.make_one(&job, told, &mut engine, |job, engine| self.file_for(job, engine));
        }
    }

    /// Makes one message with `file_for` and tells whoever asked.
    /// Viewers write the text (`!tts`): a panic in making it (misaki, ONNX
    /// Runtime) leaves that message out, not the worker, which would stall
    /// the queue waiting for it.
    fn make_one<E>(
        &self,
        job: &Job,
        told: Told,
        engine: &mut Option<E>,
        file_for: impl FnOnce(&Job, &mut Option<E>) -> anyhow::Result<(String, f64)>,
    ) {
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file_for(job, &mut *engine)));
        let Ok(made) = made else {
            tracing::error!(text = job.text, "making a message crashed; it's left out");
            // Loaded afresh for the next one, in case it was left broken.
            *engine = None;
            told.tell(None);
            return;
        };
        if let Err(error) = &made {
            tracing::warn!("couldn't read a message aloud: {error:#}");
            if engine.is_none() {
                self.status.set_speech(SpeechStatus::Failed { error: format!("{error:#}") });
            }
        }
        told.tell(
            made.ok().map(|(file, seconds)| Made { file, duration_ms: Some((seconds * 1000.0) as u32) }),
        );
    }

    /// Waits for the next message to make.
    fn next_job(&self) -> Option<(Job, Told)> {
        let mut queue = self.queue.lock().ok()?;
        loop {
            if let Some(next) = queue.pop_front() {
                return Some(next);
            }
            queue = self.wake.wait(queue).ok()?;
        }
    }

    /// The message's WAV file (made now, or made before) and its length.
    fn file_for(&self, job: &Job, engine: &mut Option<Kokoro>) -> anyhow::Result<(String, f64)> {
        let name = format!("{:016x}.wav", job_hash(job));
        let path = self.out.join(&name);
        let bytes = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(_) => {
                if engine.is_none() {
                    let started = std::time::Instant::now();
                    *engine = Some(Kokoro::load(Files::in_dir(&self.dir))?);
                    tracing::info!(took_ms = started.elapsed().as_millis() as u64, "text to speech loaded");
                }
                let kokoro = engine.as_mut().expect("loaded above");
                let started = std::time::Instant::now();
                let samples = kokoro.speak(&job.text, &job.voice, job.speed)?;
                tracing::debug!(
                    took_ms = started.elapsed().as_millis() as u64,
                    seconds = samples.len() as f64 / f64::from(SAMPLE_RATE),
                    "message spoken"
                );
                let wav = kokoro::wav(&samples);
                std::fs::create_dir_all(&self.out)?;
                std::fs::write(&path, &wav)?;
                prune(&self.out);
                wav.len() as u64
            }
        };
        let seconds = bytes.saturating_sub(44) as f64 / 2.0 / f64::from(SAMPLE_RATE);
        Ok((name, seconds))
    }
}

/// Names a message's file: the same message isn't made twice.
fn job_hash(job: &Job) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&job.text, &job.voice, job.speed.to_bits()).hash(&mut hasher);
    hasher.finish()
}

/// A WAV file's length, from its `fmt ` and `data` chunks.
pub fn wav_duration_ms(wav: &[u8]) -> Option<u32> {
    if wav.get(0..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let u32_at = |at: usize| wav.get(at..at + 4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
    let (mut at, mut byte_rate, mut data) = (12, None, None);
    while at + 8 <= wav.len() {
        let size = u32_at(at + 4)? as usize;
        match &wav[at..at + 4] {
            b"fmt " => byte_rate = u32_at(at + 16),
            b"data" => data = Some(size.min(wav.len() - at - 8)),
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    let byte_rate = u64::from(byte_rate.filter(|rate| *rate > 0)?);
    Some((data? as u64 * 1000 / byte_rate) as u32)
}

/// Keeps the newest spoken messages.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    if files.len() <= KEEP_FILES {
        return;
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in files.into_iter().skip(KEEP_FILES) {
        let _ = std::fs::remove_file(path);
    }
}

/// A spoken message's file name: 16 hex digits, and `.wav` (or `.mp3`
/// or `.ogg`, from a plugin).
fn valid_name(name: &str) -> bool {
    name.split_once('.').is_some_and(|(stem, format)| {
        FORMATS.contains(&format) && stem.len() == 16 && stem.chars().all(|c| c.is_ascii_hexdigit())
    })
}

pub async fn serve(State(state): State<AppState>, UrlPath(name): UrlPath<String>) -> Response {
    let Some(dir) = state.panel.speech.out_dir() else { return StatusCode::NOT_FOUND.into_response() };
    if !valid_name(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(dir.join(&name)).await {
        Ok(bytes) => {
            let kind = match name.rsplit('.').next() {
                Some("mp3") => "audio/mpeg",
                Some("ogg") => "audio/ogg",
                _ => "audio/wav",
            };
            ([(header::CONTENT_TYPE, kind)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_file_names() {
        assert!(valid_name("0123456789abcdef.wav"));
        assert!(!valid_name("../secret.wav"));
        assert!(valid_name("0123456789abcdef.mp3"));
        assert!(!valid_name("0123456789abcdef.exe"));
        assert!(!valid_name("0123456789abcdef.wav.mp3"));
    }

    /// Half a second at 8 kHz, mono, 16 bits.
    fn silence() -> Vec<u8> {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&16u32.to_le_bytes());
        for value in [1u16, 1] {
            wav.extend_from_slice(&value.to_le_bytes());
        }
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        for value in [2u16, 16] {
            wav.extend_from_slice(&value.to_le_bytes());
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.resize(wav.len() + 8000, 0);
        wav
    }

    #[test]
    fn wav_lengths() {
        assert_eq!(wav_duration_ms(&silence()), Some(500));
        assert_eq!(wav_duration_ms(&kokoro::wav(&vec![0.0; 24_000])), Some(1000));
        assert_eq!(wav_duration_ms(b"ID3 not a wav"), None);
    }

    #[tokio::test]
    async fn plugins_make_messages_in_their_voices() {
        let dir = std::env::temp_dir().join(format!("lsk_speech_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let speech = Speech::start(PathBuf::new(), dir.clone(), StatusHub::new(), reqwest::Client::new());
        let (speaker, mut asked) = mpsc::unbounded_channel();
        assert!(
            !speech.can_speak("robot:beep"),
            "the app's voices aren't installed, and the plugin isn't running"
        );
        speech.add_speaker("robot", speaker.clone());
        assert!(speech.can_speak("robot:beep"));
        assert!(!speech.can_speak("bm_george"));

        let (told, made) = oneshot::channel();
        let job = Job { text: "hi".into(), voice: "robot:beep".into(), speed: 1.2, volume: 1.0 };
        assert!(speech.make(job, move |made| told.send(made).unwrap()));
        let request: SpeakRequest = asked.recv().await.unwrap();
        assert_eq!((request.voice.as_str(), request.text.as_str(), request.speed), ("beep", "hi", 1.2));
        request.answer.send(Ok(Audio { data: silence(), format: "wav".into() })).unwrap();
        let made = made.await.unwrap().unwrap();
        assert_eq!(made.duration_ms, Some(500));
        assert!(made.file.ends_with(".wav") && speech.has(&made));

        // MP3 has no length; an unknown format and an error are nothing.
        for (answer, expected) in [
            (Ok(Audio { data: vec![1, 2, 3], format: "MP3".into() }), Some(None)),
            (Ok(Audio { data: vec![1, 2, 3], format: "exe".into() }), None),
            (Err("no".to_string()), None),
        ] {
            let (told, made) = oneshot::channel();
            let job =
                Job { text: format!("{expected:?}"), voice: "robot:beep".into(), speed: 1.0, volume: 1.0 };
            assert!(speech.make(job, move |made| told.send(made).unwrap()));
            asked.recv().await.unwrap().answer.send(answer).unwrap();
            assert_eq!(made.await.unwrap().map(|made| made.duration_ms), expected);
        }

        // Stopped: its voices fall back to the app's, which aren't installed.
        speech.remove_speaker("robot", &speaker);
        assert!(!speech.can_speak("robot:beep"));
        let job = Job { text: "hi".into(), voice: "robot:beep".into(), speed: 1.0, volume: 1.0 };
        assert!(!speech.make(job, |_| {}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Installs Kokoro into `$KOKORO_DIR` (about 420 MB the first time) and
    /// speaks into `$KOKORO_DIR/test.wav`. Run by hand:
    /// `KOKORO_DIR=... cargo test -p lsk real_kokoro -- --ignored`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads Kokoro; set KOKORO_DIR"]
    async fn real_kokoro() {
        let dir = PathBuf::from(std::env::var("KOKORO_DIR").expect("KOKORO_DIR"));
        let status = StatusHub::new();
        install::install(&reqwest::Client::new(), &dir, &status).await.expect("install");
        assert!(install::installed(&Files::in_dir(&dir)));
        let started = std::time::Instant::now();
        let mut kokoro = Kokoro::load(Files::in_dir(&dir)).expect("load");
        println!("loaded in {:?}", started.elapsed());
        let text = text::prepare(
            "Thanks for the raid, Marrtin and xQcOW! Lirik says pog, KEKW. gg, stay hydrated.",
            &protocol::SpeechSettings::default(),
        );
        // SPEECH_WORDS: other words to show, separated by commas.
        let extra = std::env::var("SPEECH_WORDS").unwrap_or_default();
        let words: Vec<&str> = if extra.is_empty() {
            vec!["Marrtin", "xQcOW", "Lirik", "pog", "KEKW", "gg", "brb", "Kappa", "TestViewer", "Pokimane"]
        } else {
            extra.split(',').collect()
        };
        for word in words {
            let us = kokoro.phonemes(word, "af_heart").unwrap();
            let gb = kokoro.phonemes(word, "bm_george").unwrap();
            println!("{word:12} US {us:24} GB {gb}");
        }
        for voice in ["bm_george", "af_heart"] {
            let started = std::time::Instant::now();
            let samples = kokoro.speak(&text, voice, 1.0).expect("speak");
            let seconds = samples.len() as f64 / f64::from(SAMPLE_RATE);
            println!("{voice}: {seconds:.1} s of speech in {:?} ({text})", started.elapsed());
            assert!(seconds > 2.0 && seconds < 20.0, "{seconds}");
            let peak = samples.iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.05, "not silent: {peak}");
            std::fs::write(dir.join(format!("test-{voice}.wav")), kokoro::wav(&samples)).unwrap();
        }
    }

    #[test]
    #[ignore = "inspects a model; set ONNX_RUNTIME and ONNX_MODEL"]
    fn model_inputs() {
        let runtime = PathBuf::from(std::env::var("ONNX_RUNTIME").unwrap());
        let model = PathBuf::from(std::env::var("ONNX_MODEL").unwrap());
        let model = onnx::Model::load(&runtime, &model, 1).unwrap();
        println!("inputs: {:?}", model.input_names().unwrap());
    }

    fn job(text: &str) -> Job {
        Job { text: text.into(), voice: "bm_george".into(), speed: 1.0, volume: 1.0 }
    }

    #[test]
    fn nothing_is_said_when_off() {
        assert!(!Speech::off().make(job("hi"), |_| {}));
        assert!(!Speech::off().ready());
        assert!(!Speech::queue_only().make(job("  "), |_| {}), "nothing to say");
    }

    /// A message whose making panics is left out (`None`), and the worker
    /// carries on; one dropped untold (a worker gone) is told `None` too.
    #[test]
    fn a_crash_leaves_out_only_that_message() {
        let speech = Speech::queue_only();
        let inner = speech.inner.as_ref().unwrap();
        let told = Arc::new(Mutex::new(Vec::new()));
        let telling = |told: &Arc<Mutex<Vec<Option<String>>>>| {
            let told = Arc::clone(told);
            Told::new(Box::new(move |made: Option<Made>| {
                told.lock().unwrap().push(made.map(|made| made.file))
            }))
        };
        let mut engine = Some(());
        inner.make_one(&job("crash"), telling(&told), &mut engine, |_, _| panic!("unusual input"));
        assert_eq!(engine, None, "loaded afresh next time");
        inner.make_one(&job("fine"), telling(&told), &mut engine, |_, engine| {
            *engine = Some(());
            Ok(("fine.wav".into(), 1.0))
        });
        assert_eq!(*told.lock().unwrap(), [None, Some("fine.wav".to_string())]);
        assert_eq!(inner.status.speech(), SpeechStatus::Ready, "speech still works");

        drop(telling(&told));
        assert_eq!(told.lock().unwrap().last(), Some(&None));
    }

    #[test]
    fn messages_are_made_in_order() {
        let speech = Speech::queue_only();
        let told = Arc::new(Mutex::new(Vec::new()));
        for text in ["one", "two"] {
            let told = Arc::clone(&told);
            assert!(
                speech.make(job(text), move |made| told.lock().unwrap().push(made.map(|made| made.file)))
            );
        }
        assert_eq!(speech.queued().len(), 2);
        speech.finish(|job| Some(Made { file: format!("{}.wav", job.text), duration_ms: Some(1) }));
        assert_eq!(*told.lock().unwrap(), [Some("one.wav".to_string()), Some("two.wav".to_string())]);
    }
}
