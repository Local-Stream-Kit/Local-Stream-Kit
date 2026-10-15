//! Fans commands out to every connected overlay.
//!
//! Delivery is real-time only: a command nobody is listening for is dropped.
//! Every decision about delivery lives here, so queued delivery can be added
//! later without touching consumers or plugin pages — envelopes already
//! carry a session + id and a timestamp, and overlays announce their pages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use protocol::{Command, Envelope, ShellCommand};
use tokio::sync::broadcast;

use crate::pipeline::now_ms;

/// How many commands a slow overlay can fall behind before it skips ahead.
const BACKLOG: usize = 1024;

#[derive(Clone)]
pub struct OverlayHub {
    inner: Arc<Inner>,
}

struct Inner {
    session: u64,
    next_id: AtomicU64,
    sender: broadcast::Sender<Arc<Envelope>>,
    /// The master volume and mute, sent to every overlay on hello.
    volume: RwLock<Option<Command>>,
}

impl OverlayHub {
    pub fn new() -> OverlayHub {
        let (sender, _) = broadcast::channel(BACKLOG);
        OverlayHub {
            inner: Arc::new(Inner {
                session: rand::random(),
                next_id: AtomicU64::new(1),
                sender,
                volume: RwLock::new(None),
            }),
        }
    }

    /// Wraps a command for sending to one overlay; [`Self::send`] sends to all.
    pub fn envelope(&self, command: Command) -> Envelope {
        Envelope {
            session: self.inner.session,
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed),
            issued_at_ms: now_ms(),
            command,
        }
    }

    /// Sends a command to every connected overlay (the ones hosting its
    /// plugin's page pass it on).
    pub fn send(&self, command: Command) {
        // An error only means nobody is connected: real-time delivery drops it.
        let _ = self.inner.sender.send(Arc::new(self.envelope(command)));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Envelope>> {
        self.inner.sender.subscribe()
    }

    /// Remembers the master volume for overlays that connect later.
    pub fn set_module_config(&self, command: Command) {
        if matches!(command, Command::Shell(ShellCommand::Volume { .. })) {
            *self.inner.volume.write().unwrap() = Some(command);
        }
    }

    /// What an overlay that just said hello gets first: the master volume.
    pub fn configure_envelopes(&self) -> Vec<Envelope> {
        let volume = self.inner.volume.read().unwrap().clone();
        volume.into_iter().map(|command| self.envelope(command)).collect()
    }
}

/// Whether an overlay hosting these plugins' pages should get this
/// envelope: its pages' commands, and every shell command.
pub fn wanted_by(envelope: &Envelope, plugins: &[String]) -> bool {
    match envelope.command.plugin() {
        Some(plugin) => plugins.iter().any(|hosted| hosted == plugin),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ShellCommand;

    #[test]
    fn ids_increase_and_the_volume_follows_hello() {
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        hub.send(Command::Shell(ShellCommand::Build { id: "a".into() }));
        hub.send(Command::Shell(ShellCommand::Build { id: "a".into() }));
        let first = receiver.try_recv().unwrap();
        let second = receiver.try_recv().unwrap();
        assert_eq!(first.session, second.session);
        assert!(second.id > first.id);

        assert!(hub.configure_envelopes().is_empty());
        let volume = Command::Shell(ShellCommand::Volume { volume: 0.5, muted: true });
        hub.set_module_config(volume.clone());
        let configs = hub.configure_envelopes();
        assert_eq!(configs.iter().map(|envelope| &envelope.command).collect::<Vec<_>>(), [&volume]);
    }

    #[test]
    fn filtering_by_page() {
        let hub = OverlayHub::new();
        let shell = hub.envelope(Command::Shell(ShellCommand::Build { id: "a".into() }));
        assert!(wanted_by(&shell, &[]));
        let confetti = hub.envelope(Command::Plugin(protocol::PluginCommand {
            plugin: "confetti".into(),
            message: protocol::plugin::PluginMessage::Samples,
        }));
        assert!(wanted_by(&confetti, &["confetti".into()]));
        assert!(!wanted_by(&confetti, &["other".into()]));
    }
}
