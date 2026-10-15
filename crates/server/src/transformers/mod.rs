//! Transformers: recognise meaningful things in platform-neutral inputs (and
//! in other transformers' signals). None of them know about any platform.
//! Cards then react to their signals.

mod emotes;
mod events;

pub use emotes::{Emotes, find_emotes_in_text};
pub use events::Events;
