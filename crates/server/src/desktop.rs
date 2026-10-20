//! Opening things on the PC the app runs on, for the panel: a link in the
//! default browser, the logs folder, the Stream Deck plugin in Stream
//! Deck, an exported card in the Downloads folder. The panel is often an
//! OBS dock, where a link would open inside OBS and a download does
//! nothing. With `--no-open` (for tests), each is logged instead, and
//! downloads go to `downloads/` in the app's folder.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default)]
pub struct Desktop {
    /// `--no-open`: logged, never opened.
    log_only: bool,
}

/// What the log says instead of opening something, with `what`.
pub const NOT_OPENED: &str = "not opened (--no-open)";

/// Where downloads go with `--no-open`, in the app's folder.
pub const TEST_DOWNLOADS: &str = "downloads";

impl Desktop {
    pub fn new(log_only: bool) -> Desktop {
        Desktop { log_only }
    }

    /// Opens `what` (an address, a folder or a file) with whatever the PC
    /// opens it with, without waiting for it.
    pub fn open(&self, what: impl AsRef<OsStr>) -> std::io::Result<()> {
        let what = what.as_ref();
        if self.log_only {
            tracing::info!(what = %what.to_string_lossy(), "{NOT_OPENED}");
            return Ok(());
        }
        open::that_detached(what)
    }

    /// Shows `file` in its folder, picked out in Explorer, without waiting.
    pub fn reveal(&self, file: &Path) -> std::io::Result<()> {
        if self.log_only {
            tracing::info!(what = %file.display(), "{NOT_OPENED}");
            return Ok(());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Explorer wants the path quoted after the comma, not the whole
            // argument quoted (as `arg` would, for a space).
            std::process::Command::new("explorer")
                .raw_arg(format!("/select,\"{}\"", file.display()))
                .spawn()?;
            Ok(())
        }
        #[cfg(not(windows))]
        open::that_detached(file.parent().unwrap_or(file))
    }

    /// Saves `contents` as `name` in the Downloads folder (`name (1).json`
    /// when that's taken, as a browser does) and shows it there: where it
    /// went, or why it couldn't be saved.
    pub fn download(&self, name: &str, contents: &[u8]) -> Result<PathBuf, String> {
        let folder = if self.log_only {
            Some(std::path::absolute(TEST_DOWNLOADS).unwrap_or_else(|_| TEST_DOWNLOADS.into()))
        } else {
            downloads_folder()
        };
        let folder = folder.ok_or("couldn't find the Downloads folder")?;
        let path = save_new(&folder, name, contents)?;
        if let Err(error) = self.reveal(&path) {
            tracing::warn!("couldn't show {} in Explorer ({error})", path.display());
        }
        Ok(path)
    }
}

/// Writes `contents` as a new file called `name` in `folder`, or
/// `name (1)`, `name (2)`, … for the first that isn't there yet.
fn save_new(folder: &Path, name: &str, contents: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder)
        .map_err(|error| format!("couldn't make {} ({error})", folder.display()))?;
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for n in 0.. {
        let path = match n {
            0 => folder.join(name),
            n => folder.join(format!("{stem} ({n}){extension}")),
        };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                return file
                    .write_all(contents)
                    .map(|()| path.clone())
                    .map_err(|error| format!("couldn't save {} ({error})", path.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("couldn't save {} ({error})", path.display())),
        }
    }
    unreachable!("a free name comes before the numbers run out")
}

/// `title` as a file name: without the characters Windows doesn't allow
/// in one, at most 100 characters, and `fallback` when nothing's left or
/// it's a name Windows keeps for itself (`CON`, `NUL`, …).
pub fn file_name(title: &str, fallback: &str) -> String {
    let kept: String = title
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let kept: String = kept.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
    let kept = kept.trim_end_matches(['.', ' ']);
    let reserved = |name: &str| {
        let name = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
        matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (name.len() == 4
                && (name.starts_with("COM") || name.starts_with("LPT"))
                && name.as_bytes()[3].is_ascii_digit())
    };
    if kept.is_empty() || reserved(kept) { fallback.to_string() } else { kept.to_string() }
}

/// The user's Downloads folder, wherever they've moved it.
#[cfg(windows)]
fn downloads_folder() -> Option<PathBuf> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    /// `FOLDERID_Downloads`.
    const DOWNLOADS: Guid =
        Guid(0x374D_E290, 0x123F, 0x4565, [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B]);

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: *mut c_void);
    }

    let mut path = std::ptr::null_mut();
    // SAFETY: a known folder's id, no flags or token, and a pointer for the
    // answer, which is freed below whatever the result.
    let result = unsafe { SHGetKnownFolderPath(&DOWNLOADS, 0, std::ptr::null_mut(), &mut path) };
    let folder = (result == 0 && !path.is_null()).then(|| {
        // SAFETY: on success, a NUL-terminated wide string.
        let length = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        // SAFETY: `length` characters before the NUL.
        PathBuf::from(std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(path, length) }))
    });
    // SAFETY: allocated by the call (or null, which is allowed).
    unsafe { CoTaskMemFree(path.cast()) };
    folder.or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Downloads")))
}

#[cfg(not(windows))]
fn downloads_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_windows_takes() {
        assert_eq!(file_name("Emote rain", "Card"), "Emote rain");
        assert_eq!(file_name("Raid: alert / big?", "Card"), "Raid alert big");
        assert_eq!(file_name("  Hype…  ", "Card"), "Hype…");
        assert_eq!(file_name("Bits.", "Card"), "Bits");
        assert_eq!(file_name("***", "Card"), "Card");
        assert_eq!(file_name("con", "Card"), "Card");
        assert_eq!(file_name("Com1.txt", "Card"), "Card");
        assert_eq!(file_name("Console", "Card"), "Console");
        assert_eq!(file_name(&"x".repeat(300), "Card").len(), 100);
    }

    #[test]
    fn a_download_never_replaces_a_file() {
        let dir = std::env::temp_dir().join(format!("lsk_downloads_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = save_new(&dir, "Emote rain.json", b"1").unwrap();
        let second = save_new(&dir, "Emote rain.json", b"2").unwrap();
        let third = save_new(&dir, "Emote rain.json", b"3").unwrap();
        assert_eq!(first.file_name().unwrap(), "Emote rain.json");
        assert_eq!(second.file_name().unwrap(), "Emote rain (1).json");
        assert_eq!(third.file_name().unwrap(), "Emote rain (2).json");
        assert_eq!(std::fs::read(&first).unwrap(), b"1");
        assert_eq!(std::fs::read(&third).unwrap(), b"3");
        // A folder that can't be made (a file is in the way) says so.
        let error = save_new(&first, "x.json", b"").unwrap_err();
        assert!(error.starts_with("couldn't make"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn the_downloads_folder_is_found() {
        assert!(downloads_folder().is_some_and(|folder| folder.is_absolute()));
    }
}
