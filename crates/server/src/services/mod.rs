//! Shared services: lookups any stage can use, not part of the pipeline flow.

pub mod activity;
pub mod config_store;
pub mod emotes;
pub mod image_cache;
pub mod image_proxy;
pub mod overlay_hub;
pub mod speech;
pub mod status;

use std::sync::Arc;

use activity::Activity;
use emotes::EmoteRegistry;
use overlay_hub::OverlayHub;
use speech::Speech;
use status::StatusHub;

#[derive(Clone)]
pub struct Services {
    pub emotes: Arc<EmoteRegistry>,
    pub overlay: OverlayHub,
    pub status: StatusHub,
    pub speech: Speech,
    pub activity: Activity,
    /// The pictures and sounds folders, for settings that name a folder.
    pub media: crate::media::MediaFolders,
}
