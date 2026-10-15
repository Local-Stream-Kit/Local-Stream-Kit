//! Updates: the newest release's `latest.json` is read at start and every
//! 12 hours while the switch is on, and its signature checked. Download
//! (from Status) streams the new exe into the exe's folder, checks its
//! size and SHA-256, renames the running exe to `lsk.old.exe` (Windows
//! allows renaming a running exe, not overwriting it) and puts the new one
//! under the exe's own name: it runs from the next start, which deletes
//! `lsk.old.exe`.
//!
//! A debug build is cargo's exe in `target/`, which must never be
//! replaced: it neither checks nor replaces anything unless told where to
//! (`--update-from`, `--update-exe`, as the e2e tests do).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use protocol::config::AppConfig;
use protocol::update::{self, Feed, UpdateStatus};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use crate::services::status::StatusHub;

/// What the running exe is renamed to when a new one takes its place.
pub const OLD_EXE: &str = "lsk.old.exe";
/// The download, in the exe's folder until it's checked and put in place.
const PART: &str = "lsk.update.part";
/// How often the check runs while the app runs.
const EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// The shared client times out after 20 s; the exe is about 80 MB.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// This build's version.
pub const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// Where updates come from and what they replace.
#[derive(Debug, Clone)]
pub struct Source {
    /// Where the releases are ([`update::releases`]); `None`: never checks.
    pub releases: Option<String>,
    /// The exe a download replaces; `None`: never replaces one.
    pub exe: Option<PathBuf>,
    /// The key `latest.json` must be signed with (base64).
    pub key: String,
}

impl Source {
    /// GitHub, this exe and the built-in key in a release build; in a
    /// debug build, only what's given.
    pub fn new(releases: Option<String>, exe: Option<PathBuf>, key: Option<String>) -> Source {
        let release_build = !cfg!(debug_assertions);
        Source {
            releases: releases.or_else(|| release_build.then(update::releases)),
            exe: exe.or_else(|| if release_build { std::env::current_exe().ok() } else { None }),
            key: key.unwrap_or_else(|| update::PUBLIC_KEY.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    source: Source,
    status: StatusHub,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    status: UpdateStatus,
    /// The newer release found, checked against its signature.
    found: Option<Feed>,
}

impl Updater {
    pub fn new(http: reqwest::Client, source: Source, status: StatusHub) -> Updater {
        let updater = Updater { inner: Arc::new(Inner { http, source, status, state: Mutex::default() }) };
        if updater.inner.source.releases.is_none() {
            updater.set(|state| state.status = UpdateStatus::Development);
        }
        updater
    }

    /// Deletes what the last update left: `lsk.old.exe` (the exe it
    /// replaced, which ran until the app was closed) and a download that
    /// broke off.
    pub fn clean_up(&self) {
        let Some(exe) = &self.inner.source.exe else { return };
        remove_old(exe);
    }

    /// Checks now, then every 12 hours, while `config`'s switch is on.
    pub fn start(&self, mut config: watch::Receiver<Arc<AppConfig>>) {
        if self.inner.source.releases.is_none() {
            return;
        }
        let updater = self.clone();
        tokio::spawn(async move {
            loop {
                let on = config.borrow_and_update().updates.check;
                if on {
                    updater.check().await;
                } else {
                    updater.set(|state| {
                        if !matches!(
                            state.status,
                            UpdateStatus::Downloading { .. } | UpdateStatus::Ready { .. }
                        ) {
                            state.status = UpdateStatus::Off;
                            state.found = None;
                        }
                    });
                }
                // Until it's time, or the switch changes.
                let wait = tokio::time::sleep(EVERY);
                tokio::pin!(wait);
                loop {
                    tokio::select! {
                        () = &mut wait => break,
                        changed = config.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            if config.borrow_and_update().updates.check != on {
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    fn set(&self, change: impl FnOnce(&mut State)) {
        let status = {
            let mut state = self.inner.state.lock().unwrap();
            change(&mut state);
            state.status.clone()
        };
        self.inner.status.set_update(status);
    }

    /// Reads `latest.json`. Nothing changes while a download runs or one is
    /// ready; a found version stays shown while it's checked again.
    async fn check(&self) {
        let Some(releases) = self.inner.source.releases.clone() else { return };
        let mut busy = false;
        self.set(|state| match state.status {
            UpdateStatus::Downloading { .. } | UpdateStatus::Ready { .. } => busy = true,
            UpdateStatus::Available { .. } | UpdateStatus::Failed { .. } | UpdateStatus::CantWrite { .. } => {
            }
            _ => state.status = UpdateStatus::Checking,
        });
        if busy {
            return;
        }
        let found = self.fetch(&releases).await;
        self.set(|state| {
            if matches!(state.status, UpdateStatus::Downloading { .. } | UpdateStatus::Ready { .. }) {
                return;
            }
            match found {
                Ok(feed) if is_newer(&feed.version, RUNNING) => {
                    let version = feed.version.clone();
                    tracing::info!(version, "a new version is out");
                    // A failed download of the same version keeps saying why.
                    if state.status.version() != Some(version.as_str())
                        || matches!(state.status, UpdateStatus::Available { .. })
                    {
                        state.status = UpdateStatus::Available { version };
                    }
                    state.found = Some(feed);
                }
                Ok(feed) => {
                    tracing::debug!(latest = feed.version, "up to date");
                    state.status = UpdateStatus::UpToDate;
                    state.found = None;
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    tracing::warn!("couldn't check for a new version: {error}");
                    if matches!(state.status, UpdateStatus::Checking) {
                        state.status = UpdateStatus::CheckFailed { error };
                    }
                }
            }
        });
    }

    async fn fetch(&self, releases: &str) -> anyhow::Result<Feed> {
        let feed = self.get(&update::feed_url(releases)).await?.bytes().await?;
        let signature = self.get(&update::signature_url(releases)).await?.text().await?;
        verify(&feed, &signature, &self.inner.source.key)
    }

    async fn get(&self, url: &str) -> anyhow::Result<reqwest::Response> {
        self.inner
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("couldn't read {url}"))
    }

    /// The page of the release that's out, if one is.
    pub fn release_page(&self) -> Option<String> {
        let releases = self.inner.source.releases.as_deref()?;
        let state = self.inner.state.lock().unwrap();
        Some(update::release_url(releases, &state.found.as_ref()?.version))
    }

    /// Downloads the version that's out and puts it in the exe's place.
    /// Nothing happens while one downloads, or when none is out.
    pub async fn download(&self) {
        let mut feed = None;
        self.set(|state| {
            let can = matches!(
                state.status,
                UpdateStatus::Available { .. } | UpdateStatus::Failed { .. } | UpdateStatus::CantWrite { .. }
            );
            if let (true, Some(found)) = (can, &state.found) {
                state.status = UpdateStatus::Downloading { version: found.version.clone(), percent: 0 };
                feed = Some(found.clone());
            }
        });
        let Some(feed) = feed else { return };
        let version = feed.version.clone();
        tracing::info!(version, "downloading the new version");
        let result = self.fetch_exe(&feed).await;
        self.set(|state| {
            state.status = match result {
                Ok(()) => {
                    tracing::info!(version, "the new version is in place; it runs from the next start");
                    UpdateStatus::Ready { version }
                }
                Err(Problem::CantWrite(folder)) => {
                    tracing::warn!(
                        version,
                        "can't write to {}; download it from the release page",
                        folder.display()
                    );
                    UpdateStatus::CantWrite { version, folder: folder.display().to_string() }
                }
                Err(Problem::Failed(error)) => {
                    let error = format!("{error:#}");
                    tracing::warn!(version, "the download failed: {error}");
                    UpdateStatus::Failed { version, error }
                }
            }
        });
    }

    async fn fetch_exe(&self, feed: &Feed) -> Result<(), Problem> {
        let Some(exe) = self.inner.source.exe.clone() else {
            return Err(Problem::Failed(anyhow!("a development build doesn't replace its exe")));
        };
        let releases =
            self.inner.source.releases.clone().ok_or_else(|| anyhow!("no releases to download from"))?;
        let folder = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        let part = folder.join(PART);
        let file = match tokio::fs::File::create(&part).await {
            Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                return Err(Problem::CantWrite(folder));
            }
            other => other.with_context(|| format!("couldn't write {}", part.display()))?,
        };
        let result = self.stream(&update::exe_url(&releases, &feed.version), file, feed).await;
        let result = match result {
            Ok(()) => match swap(&part, &exe) {
                Err(error) if error.kind() == ErrorKind::PermissionDenied => Err(Problem::CantWrite(folder)),
                other => other
                    .with_context(|| format!("couldn't put the new version in place of {}", exe.display()))
                    .map_err(Problem::Failed),
            },
            Err(error) => Err(Problem::Failed(error)),
        };
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    /// Streams the exe into `file`, and checks it's what `feed` says.
    async fn stream(&self, url: &str, mut file: tokio::fs::File, feed: &Feed) -> anyhow::Result<()> {
        let mut response = self
            .inner
            .http
            .get(url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .with_context(|| format!("couldn't download {url}"))?;
        let mut hasher = Sha256::new();
        let (mut size, mut shown) = (0u64, 0u8);
        while let Some(chunk) =
            response.chunk().await.with_context(|| format!("the download of {url} broke off"))?
        {
            size += chunk.len() as u64;
            if size > feed.size {
                bail!("the download is bigger than latest.json says ({} bytes)", feed.size);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            let percent = (size * 100 / feed.size.max(1)).min(99) as u8;
            if percent != shown {
                shown = percent;
                let version = feed.version.clone();
                self.set(|state| state.status = UpdateStatus::Downloading { version, percent });
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        if size != feed.size {
            bail!("the download is {size} bytes, latest.json says {}", feed.size);
        }
        let found: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        if found != feed.sha256 {
            tracing::warn!(found, expected = feed.sha256, "the download's SHA-256 isn't latest.json's");
            bail!("the download isn't the released file (its SHA-256 isn't the one latest.json gives)");
        }
        Ok(())
    }
}

/// Why a download didn't end in place.
enum Problem {
    /// The exe's folder can't be written.
    CantWrite(PathBuf),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Problem {
    fn from(error: anyhow::Error) -> Self {
        Problem::Failed(error)
    }
}

/// Checks `latest.json`'s bytes against their signature (base64) and the
/// key (base64), then reads them.
pub fn verify(feed: &[u8], signature: &str, key: &str) -> anyhow::Result<Feed> {
    let base64 = base64::engine::general_purpose::STANDARD;
    let key =
        base64.decode(key.trim()).map_err(|_| anyhow!("this build has no key to check updates with"))?;
    let signature = base64.decode(signature.trim()).context("latest.json.sig isn't base64")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key)
        .verify(feed, &signature)
        .map_err(|_| anyhow!("latest.json isn't signed with this app's key"))?;
    let feed: Feed = serde_json::from_slice(feed).context("latest.json isn't valid")?;
    semver::Version::parse(&feed.version)
        .with_context(|| format!("latest.json's version {:?} isn't valid", feed.version))?;
    let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    if feed.sha256.len() != 64 || !feed.sha256.chars().all(hex) {
        bail!("latest.json's SHA-256 isn't 64 lowercase hex digits");
    }
    Ok(feed)
}

/// Whether version `found` is newer than `running` (semver: 0.10.0 is
/// newer than 0.9.0, and 1.0.0 than 1.0.0-rc.1).
pub fn is_newer(found: &str, running: &str) -> bool {
    match (semver::Version::parse(found), semver::Version::parse(running)) {
        (Ok(found), Ok(running)) => found > running,
        _ => false,
    }
}

/// Puts `new` in `exe`'s place: `exe` becomes `lsk.old.exe` beside it
/// (one left from before is deleted first), then `new` takes its name. If
/// that fails, `exe` is put back.
pub fn swap(new: &Path, exe: &Path) -> std::io::Result<()> {
    let old = exe.with_file_name(OLD_EXE);
    match std::fs::remove_file(&old) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::rename(exe, &old)?;
    if let Err(error) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(error);
    }
    Ok(())
}

/// Deletes `lsk.old.exe` and a broken-off download beside `exe`.
pub fn remove_old(exe: &Path) {
    let old = exe.with_file_name(OLD_EXE);
    if old == exe {
        return;
    }
    match std::fs::remove_file(&old) {
        Ok(()) => tracing::info!("deleted the previous version ({})", old.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        // Still running, perhaps; the next start tries again.
        Err(error) => tracing::info!("couldn't delete {} yet: {error}", old.display()),
    }
    let _ = std::fs::remove_file(exe.with_file_name(PART));
}

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    fn key_pair() -> (Ed25519KeyPair, String) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key().as_ref());
        (pair, public)
    }

    fn sign(pair: &Ed25519KeyPair, bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(pair.sign(bytes).as_ref())
    }

    #[test]
    fn the_release_key_is_an_ed25519_public_key() {
        let key = base64::engine::general_purpose::STANDARD.decode(update::PUBLIC_KEY).unwrap();
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn the_feed_must_be_signed_with_the_key() {
        let (pair, public) = key_pair();
        let sha256 = "ab".repeat(32);
        let feed = format!(r#"{{"version":"0.2.0","size":1234,"sha256":"{sha256}"}}"#);
        let signature = sign(&pair, feed.as_bytes());
        let read = verify(feed.as_bytes(), &format!("{signature}\n"), &public).unwrap();
        assert_eq!(read, Feed { version: "0.2.0".into(), size: 1234, sha256 });

        // A byte changed, another key (the release key too), or no key.
        let changed = feed.replace("1234", "1235");
        let error = verify(changed.as_bytes(), &signature, &public).unwrap_err();
        assert!(error.to_string().contains("isn't signed with this app's key"), "{error}");
        let (_, other) = key_pair();
        assert!(verify(feed.as_bytes(), &signature, &other).is_err());
        let error = verify(feed.as_bytes(), &signature, update::PUBLIC_KEY).unwrap_err();
        assert!(error.to_string().contains("isn't signed with this app's key"), "the release key: {error}");
        let error = verify(feed.as_bytes(), &signature, "<no key>").unwrap_err();
        assert!(error.to_string().contains("no key"), "{error}");
        assert!(verify(feed.as_bytes(), "not base64!", &public).is_err());

        // Signed, but not what a feed must be.
        for bad in [
            r#"{"version":"two","size":1,"sha256":"00"}"#.to_string(),
            format!(r#"{{"version":"0.2.0","size":1,"sha256":"{}"}}"#, "AB".repeat(32)),
            "[]".to_string(),
        ] {
            assert!(verify(bad.as_bytes(), &sign(&pair, bad.as_bytes()), &public).is_err(), "{bad}");
        }
    }

    #[test]
    fn versions_compare_as_semver() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.0"), "not as text");
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"), "an older one isn't offered");
        assert!(!is_newer("v0.2.0", "0.1.0"), "not semver");
        assert!(is_newer("0.2.0", RUNNING) || !is_newer(RUNNING, "0.2.0"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsk_update_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_new_exe_takes_the_old_ones_place() {
        let dir = temp_dir("swap");
        let (exe, new, old) = (dir.join("lsk.exe"), dir.join(PART), dir.join(OLD_EXE));
        std::fs::write(&exe, "running").unwrap();
        std::fs::write(&old, "left from before").unwrap();
        std::fs::write(&new, "new").unwrap();
        swap(&new, &exe).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "running");
        assert!(!new.exists());

        // No new file: the exe is put back.
        std::fs::write(&exe, "running").unwrap();
        assert!(swap(&new, &exe).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "running");

        // The next start deletes the old one and a broken-off download.
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "half").unwrap();
        remove_old(&exe);
        assert!(!old.exists() && !new.exists());
        assert!(exe.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_debug_build_checks_and_replaces_only_what_its_told() {
        let source = Source::new(None, None, None);
        if cfg!(debug_assertions) {
            assert!(source.releases.is_none() && source.exe.is_none());
        }
        assert_eq!(source.key, update::PUBLIC_KEY);
        let source = Source::new(Some("http://127.0.0.1:1".into()), Some("x.exe".into()), Some("k".into()));
        assert_eq!(source.releases.as_deref(), Some("http://127.0.0.1:1"));
        assert_eq!(source.exe, Some(PathBuf::from("x.exe")));
    }
}
