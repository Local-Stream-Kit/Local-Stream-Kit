//! Media for effects: pictures for alerts and sounds, each in a folder next
//! to the config (`pictures`, `sounds`), filled by uploads from the control
//! panel (or by hand), served to the overlay at
//! `protocol::alert::PICTURES_ROUTE` and `protocol::sound::SOUNDS_ROUTE`.
//! Folders in them (one level, filled by hand) are for settings that name
//! one (`"alerts/"`) and get one of its files at random each time.

use std::path::{Path as FsPath, PathBuf};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use protocol::panel::MediaFolder;
use protocol::plugin::MediaKind;

use crate::panel::PanelState;
use crate::pipeline::RandomSource;
use crate::web::AppState;

/// The largest file the panel can upload (a short video clip fits).
pub const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// One kind of media: where it lives and which files count.
pub struct Media {
    /// "picture" / "sound", for messages.
    what: &'static str,
    /// File types by extension.
    types: &'static [(&'static str, &'static str)],
    /// For the upload error: the extensions in words.
    allowed: &'static str,
    /// Whether the bytes start like such a file (so the folder only ever
    /// gets these, whatever a request claims).
    looks_right: fn(&[u8]) -> bool,
    folder: fn(&MediaFolders) -> &PathBuf,
}

pub const PICTURES: Media = Media {
    what: "picture",
    types: &[
        ("png", "image/png"),
        ("apng", "image/apng"),
        ("gif", "image/gif"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("webp", "image/webp"),
        ("avif", "image/avif"),
        // For Show media (`protocol::alert::VIDEO_EXTENSIONS`).
        ("webm", "video/webm"),
        ("mp4", "video/mp4"),
    ],
    allowed: ".png, .gif, .jpg, .webp, .avif, .webm or .mp4",
    looks_right: |bytes| {
        bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || bytes.starts_with(b"GIF87a")
            || bytes.starts_with(b"GIF89a")
            || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
            || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
            // AVIF and MP4 are both ISO media files.
            || (bytes.len() >= 12 && &bytes[4..8] == b"ftyp")
            // WebM (Matroska).
            || bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
    },
    folder: |folders| &folders.pictures,
};

pub const SOUNDS: Media = Media {
    what: "sound",
    types: &[
        ("mp3", "audio/mpeg"),
        ("wav", "audio/wav"),
        ("ogg", "audio/ogg"),
        ("oga", "audio/ogg"),
        ("opus", "audio/ogg"),
        ("flac", "audio/flac"),
        ("m4a", "audio/mp4"),
        ("webm", "audio/webm"),
    ],
    allowed: ".mp3, .wav, .ogg, .opus, .flac, .m4a or .webm",
    looks_right: |bytes| {
        bytes.starts_with(b"ID3")
            // An MP3 frame without tags: 11 sync bits.
            || (bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
            || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE")
            || bytes.starts_with(b"OggS")
            || bytes.starts_with(b"fLaC")
            || (bytes.len() >= 8 && &bytes[4..8] == b"ftyp")
            || bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
    },
    folder: |folders| &folders.sounds,
};

impl Media {
    fn content_type(&self, name: &str) -> Option<&'static str> {
        let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
        self.types.iter().find(|(known, _)| *known == extension).map(|(_, mime)| *mime)
    }

    /// A safe file name for an upload: no folders, only plain characters
    /// (others become `_`), and one of this kind's extensions.
    pub fn clean_name(&self, name: &str) -> Option<String> {
        let name = name.rsplit(['/', '\\']).next()?.trim();
        let clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let clean = clean.trim_start_matches('.').trim().to_string();
        (!clean.is_empty() && clean.len() <= 120 && self.content_type(&clean).is_some()).then_some(clean)
    }

    /// The files in the folder, sorted ignoring case.
    pub fn list(&self, dir: &FsPath) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut files: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| self.clean_name(name).as_deref() == Some(name.as_str()))
            .collect();
        files.sort_by_key(|name| name.to_lowercase());
        files
    }

    /// The folders in the folder that have files, each with its files,
    /// sorted ignoring case.
    pub fn folders(&self, dir: &FsPath) -> Vec<MediaFolder> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut folders: Vec<MediaFolder> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_folder_name(name))
            .map(|name| MediaFolder { files: self.list(&dir.join(&name)), name })
            .filter(|folder| !folder.files.is_empty())
            .collect();
        folders.sort_by_key(|folder| folder.name.to_lowercase());
        folders
    }

    /// The files and the folders, for the panel.
    pub async fn list_in(&'static self, panel: &PanelState) -> (Vec<String>, Vec<MediaFolder>) {
        let dir = (self.folder)(&panel.media).clone();
        tokio::task::spawn_blocking(move || (self.list(&dir), self.folders(&dir))).await.unwrap_or_default()
    }

    /// A file in the folder, or in one of its folders (`"alerts/hype.gif"`).
    fn path_of(&self, dir: &FsPath, name: &str) -> Option<PathBuf> {
        let file_in = |dir: &FsPath, name: &str| {
            let clean = self.clean_name(name)?;
            (clean == name).then(|| dir.join(clean))
        };
        match name.split_once('/') {
            Some((folder, name)) => file_in(&dir.join(is_folder_name(folder).then_some(folder)?), name),
            None => file_in(dir, name),
        }
    }

    async fn serve(&self, panel: &PanelState, name: &str) -> Response {
        let Some(path) = self.path_of((self.folder)(&panel.media), name) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match tokio::fs::read(&path).await {
            Ok(bytes) => (
                [
                    (header::CONTENT_TYPE, self.content_type(name).unwrap_or("application/octet-stream")),
                    // An upload can replace a file under the same name.
                    (header::CACHE_CONTROL, "no-cache"),
                    // Plugin pages may decode them.
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, crate::web::SANDBOXED),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// Deletes a file in the folder (not in one of its folders), as the
    /// panel asks; why not, if it can't.
    pub async fn delete(&self, folders: &MediaFolders, name: &str) -> Result<(), String> {
        let dir = (self.folder)(folders);
        let path = (!name.contains('/'))
            .then(|| self.path_of(dir, name))
            .flatten()
            .ok_or_else(|| format!("{name:?} isn't a {} in the {}s folder", self.what, self.what))?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                tracing::info!(name, "{} deleted", self.what);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(name, %error, "couldn't delete a {}", self.what);
                Err(format!("couldn't delete the {} {name:?}: {error}", self.what))
            }
        }
    }

    /// `POST` from the panel: the file's bytes as the body. The panel sends
    /// a custom header, which a web page elsewhere can't send to localhost
    /// without the server agreeing (CORS), so other sites can't upload.
    async fn upload(&self, panel: &PanelState, name: &str, headers: &HeaderMap, body: &Bytes) -> Response {
        if !headers.contains_key(protocol::panel::UPLOAD_HEADER) {
            return (StatusCode::FORBIDDEN, "uploads only come from the control panel").into_response();
        }
        let Some(name) = self.clean_name(name) else {
            return (StatusCode::BAD_REQUEST, format!("{}s must be {}", self.what, self.allowed))
                .into_response();
        };
        if !(self.looks_right)(body) {
            return (StatusCode::BAD_REQUEST, format!("that file isn't a {}", self.what)).into_response();
        }
        let dir = (self.folder)(&panel.media);
        let saved = async {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::write(dir.join(&name), body).await
        };
        match saved.await {
            Ok(()) => {
                tracing::info!(name, bytes = body.len(), "{} uploaded", self.what);
                name.into_response()
            }
            Err(error) => {
                tracing::warn!(name, %error, "couldn't save an uploaded {}", self.what);
                (StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't save the {}", self.what))
                    .into_response()
            }
        }
    }
}

/// A folder's name as a setting can give it: the characters a file name
/// may have, not starting with a dot.
fn is_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && name.trim() == name
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')'))
}

/// The pictures and sounds folders.
#[derive(Debug, Clone, Default)]
pub struct MediaFolders {
    pub pictures: PathBuf,
    pub sounds: PathBuf,
}

impl MediaFolders {
    /// One of the files in `folder` (in the pictures or sounds folder) that
    /// `kind` takes, at random, as `"alerts/hype.gif"`; `None` for an empty
    /// or missing folder.
    pub fn pick(&self, kind: MediaKind, folder: &str, rng: &mut dyn RandomSource) -> Option<String> {
        let files = self.files(kind, folder);
        (!files.is_empty()).then(|| files[rng.index(files.len())].clone())
    }

    /// Every file in `folder` that `kind` takes, in name order, as
    /// `"alerts/hype.gif"`.
    pub fn files(&self, kind: MediaKind, folder: &str) -> Vec<String> {
        let media = match kind {
            MediaKind::Picture | MediaKind::Media => &PICTURES,
            MediaKind::Sound => &SOUNDS,
        };
        if !is_folder_name(folder) {
            return Vec::new();
        }
        media
            .list(&(media.folder)(self).join(folder))
            .into_iter()
            .filter(|file| kind.takes(file))
            .map(|file| format!("{folder}/{file}"))
            .collect()
    }

    /// Files for a setting naming `folder` (`protocol::plugin::PickFile`):
    /// one at random, or with `every` all of them.
    pub fn pick_files(
        &self,
        kind: MediaKind,
        folder: &str,
        every: bool,
        rng: &mut dyn RandomSource,
    ) -> Vec<String> {
        if every { self.files(kind, folder) } else { self.pick(kind, folder, rng).into_iter().collect() }
    }

    /// [`pick_files`](Self::pick_files), outside the pipeline.
    pub fn pick_files_any(&self, kind: MediaKind, folder: &str, every: bool) -> Vec<String> {
        self.pick_files(kind, folder, every, &mut crate::pipeline::StdRandom::new())
    }
}

pub async fn serve_picture(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    PICTURES.serve(&state.panel, &name).await
}

pub async fn serve_sound(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    SOUNDS.serve(&state.panel, &name).await
}

pub async fn upload_picture(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    PICTURES.upload(&state.panel, &name, &headers, &body).await
}

pub async fn upload_sound(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    SOUNDS.upload(&state.panel, &name, &headers, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(PICTURES.clean_name("hype.gif").as_deref(), Some("hype.gif"));
        assert_eq!(PICTURES.clean_name("C:\\Users\\me\\Big Hype!.GIF").as_deref(), Some("Big Hype_.GIF"));
        assert_eq!(PICTURES.clean_name("../../secret.png").as_deref(), Some("secret.png"));
        assert_eq!(PICTURES.clean_name("..png"), None, "no extension left");
        assert_eq!(PICTURES.clean_name("notes.txt"), None);
        assert_eq!(PICTURES.clean_name(".gif"), None);
        assert_eq!(PICTURES.clean_name("horn.mp3"), None, "a sound isn't a picture");
        assert_eq!(SOUNDS.clean_name("Air horn!.MP3").as_deref(), Some("Air horn_.MP3"));
        assert!(PICTURES.path_of(FsPath::new("p"), "a\\b.png").is_none());
    }

    #[tokio::test]
    async fn a_sound_is_deleted() {
        let dir = std::env::temp_dir().join(format!("lsk_media_delete_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("alerts")).unwrap();
        for name in ["horn.mp3", "alerts/ding.mp3", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let folders = MediaFolders { pictures: dir.clone(), sounds: dir.clone() };
        SOUNDS.delete(&folders, "horn.mp3").await.unwrap();
        assert!(!dir.join("horn.mp3").exists());
        assert!(SOUNDS.delete(&folders, "horn.mp3").await.is_err(), "gone already");
        for name in ["alerts/ding.mp3", "../horn.mp3", "notes.txt", "alerts"] {
            assert!(SOUNDS.delete(&folders, name).await.is_err(), "{name}");
        }
        assert!(dir.join("alerts").join("ding.mp3").exists() && dir.join("notes.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_right_files_are_kept() {
        let picture = PICTURES.looks_right;
        assert!(picture(b"GIF89a......"));
        assert!(picture(b"\x89PNG\r\n\x1a\n...."));
        assert!(picture(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!picture(b"<html>"));
        let sound = SOUNDS.looks_right;
        assert!(sound(b"ID3\x04\0"));
        assert!(sound(&[0xFF, 0xFB, 0x90, 0x00]));
        assert!(sound(b"RIFF\0\0\0\0WAVEfmt "));
        assert!(sound(b"OggS\0"));
        assert!(!sound(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!sound(b"MZ\x90\0"));
        let dir = std::env::temp_dir().join(format!("lsk_media_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.png", "A.gif", "notes.txt", "horn.mp3"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(PICTURES.list(&dir), ["A.gif", "b.png"]);
        assert_eq!(SOUNDS.list(&dir), ["horn.mp3"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folders_give_a_file_at_random() {
        let dir = std::env::temp_dir().join(format!("lsk_media_folders_{}", std::process::id()));
        for (folder, name) in
            [("alerts", "b.png"), ("alerts", "a.gif"), ("alerts", "clip.webm"), ("Empty", "x.txt")]
        {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
            std::fs::write(dir.join(folder).join(name), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".hidden").join("c.png"), b"x").unwrap();
        assert_eq!(
            PICTURES.folders(&dir),
            [MediaFolder {
                name: "alerts".into(),
                files: vec!["a.gif".into(), "b.png".into(), "clip.webm".into()]
            }],
            "only folders with files of its kind, and none starting with a dot"
        );
        assert_eq!(PICTURES.path_of(&dir, "alerts/a.gif"), Some(dir.join("alerts").join("a.gif")));
        assert_eq!(PICTURES.path_of(&dir, "../alerts/a.gif"), None);
        assert_eq!(PICTURES.path_of(&dir, "alerts/../a.gif"), None);
        assert_eq!(PICTURES.path_of(&dir, "a/b/c.gif"), None, "one level only");

        let folders = MediaFolders { pictures: dir.clone(), sounds: dir.clone() };
        let mut rng = crate::pipeline::testing::FixedRandom::new(&[0.0, 0.99]);
        assert_eq!(folders.pick(MediaKind::Picture, "alerts", &mut rng).as_deref(), Some("alerts/a.gif"));
        assert_eq!(
            folders.pick(MediaKind::Picture, "alerts", &mut rng).as_deref(),
            Some("alerts/b.png"),
            "a picture setting skips videos"
        );
        let mut rng = crate::pipeline::testing::FixedRandom::new(&[0.99]);
        assert_eq!(folders.pick(MediaKind::Media, "alerts", &mut rng).as_deref(), Some("alerts/clip.webm"));
        assert_eq!(folders.pick(MediaKind::Sound, "Empty", &mut rng), None);
        assert_eq!(folders.pick(MediaKind::Picture, "missing", &mut rng), None);
        assert_eq!(folders.pick(MediaKind::Picture, "..", &mut rng), None);
        assert_eq!(
            folders.pick_files(MediaKind::Picture, "alerts", true, &mut rng),
            ["alerts/a.gif", "alerts/b.png"]
        );
        assert_eq!(folders.pick_files(MediaKind::Picture, "missing", true, &mut rng), Vec::<String>::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
