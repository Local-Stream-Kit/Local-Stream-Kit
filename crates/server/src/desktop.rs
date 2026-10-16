//! Opening things on the PC the app runs on, for the panel: a link in the
//! default browser, the logs folder, the Stream Deck plugin in Stream
//! Deck. The panel is often an OBS dock, where a link would open inside
//! OBS. With `--no-open` (for tests), each is logged instead.

use std::ffi::OsStr;

#[derive(Debug, Clone, Copy, Default)]
pub struct Desktop {
    /// `--no-open`: logged, never opened.
    log_only: bool,
}

/// What the log says instead of opening something, with `what`.
pub const NOT_OPENED: &str = "not opened (--no-open)";

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
}
