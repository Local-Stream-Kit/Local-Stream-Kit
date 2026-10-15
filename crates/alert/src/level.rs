//! The master volume and mute (from the overlay shell), and the sounds
//! playing with their own volumes, so a change reaches what's playing.

use std::cell::RefCell;

use web_sys::HtmlAudioElement;

struct Level {
    volume: f32,
    muted: bool,
    playing: Vec<(HtmlAudioElement, f32)>,
}

thread_local! {
    static LEVEL: RefCell<Level> = const { RefCell::new(Level { volume: 1.0, muted: false, playing: Vec::new() }) };
}

/// The master volume (0–1) and mute, for everything playing and to come.
pub fn set(volume: f32, muted: bool) {
    LEVEL.with_borrow_mut(|level| {
        level.volume = volume.clamp(0.0, 1.0);
        level.muted = muted;
        // Over, or stopped (skipped): no need to follow any more.
        level.playing.retain(|(audio, _)| !audio.ended() && !audio.paused() && audio.error().is_none());
        for (audio, own) in &level.playing {
            apply(level, audio, *own);
        }
    });
}

fn apply(level: &Level, audio: &HtmlAudioElement, own: f32) {
    audio.set_volume(f64::from(own.clamp(0.0, 1.0) * level.volume));
    audio.set_muted(level.muted);
}

/// A sound loading ahead of time, for [`start`] later.
pub fn preload(url: &str) -> Option<HtmlAudioElement> {
    let audio = HtmlAudioElement::new_with_src(url).ok()?;
    audio.set_preload("auto");
    Some(audio)
}

/// Plays `audio` at its own `volume` (0–1) times the master volume.
pub fn start(audio: &HtmlAudioElement, volume: f32) {
    LEVEL.with_borrow_mut(|level| {
        apply(level, audio, volume);
        level.playing.retain(|(audio, _)| !audio.ended() && audio.error().is_none());
        level.playing.push((audio.clone(), volume));
    });
    // A page nobody has clicked in may not play sound (OBS always may).
    if let Err(error) = audio.play() {
        super::warn(&format!("couldn't play a sound: {error:?}"));
    }
}
