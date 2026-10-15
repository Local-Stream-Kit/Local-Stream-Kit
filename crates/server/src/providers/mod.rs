//! Providers: the only stages that know a platform. Each one connects, stays
//! connected, and translates everything into the platform-neutral model.

pub mod replay;
pub mod twitch_chat;
pub mod twitch_eventsub;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::model::Input;
use crate::services::Services;

pub trait Provider: Send {
    fn spawn(self: Box<Self>, inputs: mpsc::Sender<Input>, services: Services) -> JoinHandle<()>;
}
