//! Updates: where the releases are, the update feed's format, and what
//! Status shows about them.
//!
//! Every release has `lsk.exe`, `latest.json` ([`Feed`]) and
//! `latest.json.sig`: the base64 of the ed25519 signature over
//! `latest.json`'s exact bytes, made with the key whose public half is
//! [`PUBLIC_KEY`].

use serde::{Deserialize, Serialize};

/// The GitHub organization and repo the source and the releases are in,
/// built into every exe, so it doesn't move.
pub const REPO: &str = "Local-Stream-Kit/Local-Stream-Kit";

/// Where "Support the project" goes (PayPal.Me). `.github/FUNDING.yml`
/// has the same link.
pub const SUPPORT_URL: &str = "https://paypal.me/marhag87";

/// The public half of the key `latest.json` is signed with: standard
/// base64 of the 32 bytes of an ed25519 public key. Made with
/// `cargo keygen` on 2026-10-15; the private half is the `Release`
/// environment's `LSK_RELEASE_KEY` secret on GitHub.
pub const PUBLIC_KEY: &str = "3dR2sW4Cls8xe35WKuqCTusmskCK4A9McAK1u7yhkL4=";

/// The licence's name, for the link to `LICENSE.md`.
pub const LICENCE: &str = "PolyForm Perimeter License 1.0.1";

/// `LICENSE.md`'s `Required Notice:` line: whoever gets a copy must get it
/// too, and the exe is downloaded alone, so Status shows it.
pub const REQUIRED_NOTICE: &str = "Required Notice: Copyright 2026 Martin Hagström";

/// Where the releases are: the repo on GitHub.
pub fn releases() -> String {
    format!("https://github.com/{REPO}")
}

/// `LICENSE.md` on GitHub.
pub fn licence_url() -> String {
    format!("{}/blob/main/LICENSE.md", releases())
}

/// The newest published release's `latest.json`, under `releases`
/// ([`releases`], or a stand-in's address in tests). GitHub redirects it
/// to the release's file; drafts and pre-releases never count as latest.
pub fn feed_url(releases: &str) -> String {
    format!("{releases}/releases/latest/download/latest.json")
}

/// Its signature.
pub fn signature_url(releases: &str) -> String {
    format!("{}.sig", feed_url(releases))
}

/// The exe of the release `version`.
pub fn exe_url(releases: &str, version: &str) -> String {
    format!("{releases}/releases/download/v{version}/lsk.exe")
}

/// The release's page: its notes ("What's new") and its files.
pub fn release_url(releases: &str, version: &str) -> String {
    format!("{releases}/releases/tag/v{version}")
}

/// `latest.json`: the newest release's version, and its `lsk.exe`'s size
/// and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    /// Semver, without a `v`: `0.2.0`.
    pub version: String,
    /// In bytes.
    pub size: u64,
    /// 64 lowercase hex digits.
    pub sha256: String,
}

/// Where the update check and download are, for Status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// Checking is switched off (`updates.check`).
    #[default]
    Off,
    /// A development build (cargo's, in `target/`): it never checks or
    /// replaces itself.
    Development,
    Checking,
    UpToDate,
    /// The check didn't work (no network, a bad signature); it's tried
    /// again in 12 hours.
    CheckFailed {
        error: String,
    },
    /// A newer version is out.
    Available {
        version: String,
    },
    Downloading {
        version: String,
        percent: u8,
    },
    /// Downloaded and in the exe's place: it runs from the next start.
    Ready {
        version: String,
    },
    /// The download (or putting it in place) failed; Download tries again.
    Failed {
        version: String,
        error: String,
    },
    /// The exe's folder can't be written (Program Files): the release page
    /// has the new exe.
    CantWrite {
        version: String,
        folder: String,
    },
}

impl UpdateStatus {
    /// The newer version that's out, if one was found.
    pub fn version(&self) -> Option<&str> {
        match self {
            UpdateStatus::Available { version }
            | UpdateStatus::Downloading { version, .. }
            | UpdateStatus::Ready { version }
            | UpdateStatus::Failed { version, .. }
            | UpdateStatus::CantWrite { version, .. } => Some(version),
            _ => None,
        }
    }
}

/// A page the panel asks the app to open in the browser
/// (`PanelRequest::OpenAppLink`): the app builds the address itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLink {
    /// The page of the release that's out ("What's new").
    WhatsNew,
    /// `LICENSE.md` on GitHub.
    Licence,
    /// "Support the project".
    Support,
    /// The Stream Deck plugin's file in this version's release, for a
    /// build without it built in.
    StreamDeckPlugin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_notice_is_licence_mds() {
        let licence = include_str!("../../../LICENSE.md");
        assert!(licence.lines().any(|line| line == REQUIRED_NOTICE));
        assert!(licence.contains(LICENCE));
    }

    #[test]
    fn addresses() {
        let at = "http://127.0.0.1:9";
        assert_eq!(feed_url(at), "http://127.0.0.1:9/releases/latest/download/latest.json");
        assert_eq!(signature_url(at), "http://127.0.0.1:9/releases/latest/download/latest.json.sig");
        assert_eq!(exe_url(at, "0.2.0"), "http://127.0.0.1:9/releases/download/v0.2.0/lsk.exe");
        assert_eq!(release_url(at, "0.2.0"), "http://127.0.0.1:9/releases/tag/v0.2.0");
        assert!(licence_url().ends_with("/blob/main/LICENSE.md"));
    }

    #[test]
    fn the_feeds_shape() {
        let feed = Feed { version: "0.2.0".into(), size: 5, sha256: "ab".into() };
        assert_eq!(serde_json::to_string(&feed).unwrap(), r#"{"version":"0.2.0","size":5,"sha256":"ab"}"#);
        let status = UpdateStatus::Downloading { version: "0.2.0".into(), percent: 40 };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({ "state": "downloading", "version": "0.2.0", "percent": 40 })
        );
        assert_eq!(status.version(), Some("0.2.0"));
        assert_eq!(UpdateStatus::UpToDate.version(), None);
    }
}
