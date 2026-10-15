//! How alerts look: named styles, picked per alert node. The overlay and the
//! control panel's preview draw an alert the same way, from the class names
//! and CSS variables made here and the CSS in `crates/overlay/alert.css`:
//!
//! ```html
//! <div class="alert-frame" style="--x: 0.5; --y: 0">  <!-- full size, its margin -->
//!  <div class="alert-room">                  <!-- inside the margin; places the alert -->
//!   <div class="alert anim-slide-down picture-above" style="--alert-…">
//!     <img class="alert-picture" src="/pictures/hype.gif">
//!     <div class="alert-text">Viewer cheered 100 bits</div>
//!     <!-- or, with emotes (`AlertPart`): -->
//!     <div class="alert-text"><span>Viewer: </span><span class="alert-emote"><img alt="Kappa"></span></div>
//!   </div>
//!  </div>
//! </div>
//! ```

use serde::{Deserialize, Serialize};

use crate::plugin::Position;
use crate::text_style::{FALLBACK_FONTS, font_family, font_stylesheet, number};

/// Pictures uploaded from the panel are served here, in axum path syntax.
/// See [`picture_path`].
pub const PICTURES_ROUTE: &str = "/pictures/{name}";

/// The path the overlay loads a picture from.
pub fn picture_path(name: &str) -> String {
    format!("/pictures/{}", crate::encode_segment(name))
}

/// Videos the pictures folder also takes (for plugins' `media` settings,
/// such as Show media); alerts only show pictures.
pub const VIDEO_EXTENSIONS: [&str; 2] = ["webm", "mp4"];

/// Whether a file in the pictures folder is a video, by its extension.
pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        VIDEO_EXTENSIONS.iter().any(|video| video.eq_ignore_ascii_case(extension))
    })
}

/// How the alert comes and goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertAnimation {
    /// In from above, out the same way.
    #[default]
    SlideDown,
    /// In from below, out the same way.
    SlideUp,
    Fade,
    /// Grows in with a bounce, shrinks out.
    Pop,
    None,
}

impl AlertAnimation {
    pub const ALL: [AlertAnimation; 5] = [
        AlertAnimation::SlideDown,
        AlertAnimation::SlideUp,
        AlertAnimation::Fade,
        AlertAnimation::Pop,
        AlertAnimation::None,
    ];

    pub fn title(self) -> &'static str {
        match self {
            AlertAnimation::SlideDown => "Slide down",
            AlertAnimation::SlideUp => "Slide up",
            AlertAnimation::Fade => "Fade",
            AlertAnimation::Pop => "Pop",
            AlertAnimation::None => "None",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            AlertAnimation::SlideDown => "anim-slide-down",
            AlertAnimation::SlideUp => "anim-slide-up",
            AlertAnimation::Fade => "anim-fade",
            AlertAnimation::Pop => "anim-pop",
            AlertAnimation::None => "anim-none",
        }
    }

    /// How long leaving takes; matches `alert.css`.
    pub fn leave_ms(self) -> u32 {
        match self {
            AlertAnimation::None => 0,
            _ => 400,
        }
    }
}

/// Where the picture goes, next to the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicturePlacement {
    #[default]
    Above,
    /// Since 2026-10-15, with Right (asked: an alert can be anywhere now).
    Below,
    Left,
    Right,
}

impl PicturePlacement {
    pub const ALL: [PicturePlacement; 4] =
        [PicturePlacement::Above, PicturePlacement::Below, PicturePlacement::Left, PicturePlacement::Right];

    /// Where the picture goes, beside the text.
    pub fn title(self) -> &'static str {
        match self {
            PicturePlacement::Above => "Above",
            PicturePlacement::Below => "Below",
            PicturePlacement::Left => "Left",
            PicturePlacement::Right => "Right",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            PicturePlacement::Above => "picture-above",
            PicturePlacement::Below => "picture-below",
            PicturePlacement::Left => "picture-left",
            PicturePlacement::Right => "picture-right",
        }
    }
}

/// One alert look. Alert nodes pick a style by `id`; an unknown id gets the
/// first style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertStyle {
    /// Stable, unique among styles.
    pub id: String,
    pub name: String,
    /// Where it sits: one of the nine names, or x and y in %.
    pub position: Position,
    pub animation: AlertAnimation,
    /// A font installed on this PC, or a Google Fonts family (loaded when
    /// the overlay is online); empty = the system's.
    pub font: String,
    pub font_size_px: f32,
    pub bold: bool,
    pub italic: bool,
    /// Emote codes in the text shown as the emotes, as in chat.
    pub emotes: bool,
    /// `#rrggbb`.
    pub text_color: String,
    /// The box behind the text, `#rrggbb`.
    pub background: String,
    /// 0 = no box: only the text (with its shadow) and the picture.
    pub background_opacity: f32,
    pub corner_radius_px: f32,
    /// A file in the pictures folder, or an http(s) address; empty = none.
    pub picture: String,
    pub picture_height_px: f32,
    pub picture_placement: PicturePlacement,
    /// Plays when the alert shows: a file in the sounds folder, or an
    /// http(s) address; empty = none.
    pub sound: String,
    /// 0–1 (times the master volume).
    pub sound_volume: f32,
}

impl Default for AlertStyle {
    /// The look alerts had before styles: white bold text in a dark box,
    /// sliding down at the top.
    fn default() -> Self {
        AlertStyle {
            id: "default".into(),
            name: "Default".into(),
            position: Position::TOP,
            animation: AlertAnimation::SlideDown,
            font: String::new(),
            font_size_px: 38.0,
            bold: true,
            italic: false,
            emotes: true,
            text_color: "#ffffff".into(),
            background: "#14141e".into(),
            background_opacity: 0.85,
            corner_radius_px: 14.0,
            picture: String::new(),
            picture_height_px: 200.0,
            picture_placement: PicturePlacement::Above,
            sound: String::new(),
            sound_volume: 0.8,
        }
    }
}

impl AlertStyle {
    /// The frame's `style` attribute: where in the room the alert goes,
    /// across and down, each 0–1 (`alert.css` places it so it stays inside).
    pub fn frame_css(&self) -> String {
        let (x, y) = self.position.fractions();
        format!("--x: {x}; --y: {y}")
    }

    /// The alert box's classes.
    pub fn classes(&self) -> String {
        format!("alert {} {}", self.animation.class(), self.picture_placement.class())
    }

    /// The alert box's `style` attribute: CSS variables only. Every value is
    /// checked or rebuilt here, so a config value can't inject CSS.
    pub fn css(&self) -> String {
        let family = font_family(&self.font).unwrap_or_else(|| FALLBACK_FONTS.to_string());
        let (r, g, b) = parse_hex(&self.background).unwrap_or((20, 20, 30));
        let text = parse_hex(&self.text_color).map_or("#ffffff".to_string(), |(r, g, b)| hex(r, g, b));
        format!(
            "--alert-font: {family}; --alert-size: {}px; --alert-weight: {}; --alert-style: {}; \
             --alert-color: {text}; --alert-background: rgba({r}, {g}, {b}, {}); --alert-radius: {}px; \
             --alert-picture-height: {}px",
            number(self.font_size_px, 8.0, 300.0),
            if self.bold { 700 } else { 400 },
            if self.italic { "italic" } else { "normal" },
            number(self.background_opacity, 0.0, 1.0),
            number(self.corner_radius_px, 0.0, 200.0),
            number(self.picture_height_px, 10.0, 1080.0),
        )
    }

    /// Where the picture loads from, if there is one.
    pub fn picture_url(&self) -> Option<String> {
        let picture = self.picture.trim();
        if picture.is_empty() {
            None
        } else if picture.starts_with('/')
            || picture.starts_with("https://")
            || picture.starts_with("http://")
        {
            Some(picture.to_string())
        } else {
            Some(picture_path(picture))
        }
    }

    /// The stylesheet that loads the font from Google Fonts, if it needs one.
    pub fn font_stylesheet(&self) -> Option<String> {
        font_stylesheet(&self.font)
    }
}

/// `#rrggbb` (or `#rgb`) as its three parts.
pub fn parse_hex(color: &str) -> Option<(u8, u8, u8)> {
    let digits = color.trim().strip_prefix('#')?;
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |text: &str| u8::from_str_radix(text, 16).ok();
    match digits.len() {
        6 => Some((channel(&digits[0..2])?, channel(&digits[2..4])?, channel(&digits[4..6])?)),
        3 => {
            let short = |i: usize| channel(&digits[i..i + 1]).map(|v| v * 17);
            Some((short(0)?, short(1)?, short(2)?))
        }
        _ => None,
    }
}

fn hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_is_built_from_checked_values() {
        let style = AlertStyle {
            font: "Comic Sans MS\"; } body { display: none".into(),
            text_color: "red; background: url(x)".into(),
            background: "#336699".into(),
            font_size_px: f32::NAN,
            ..AlertStyle::default()
        };
        let css = style.css();
        assert!(css.contains("--alert-font: \"Comic Sans MS  body  display none\""), "{css}");
        assert!(css.contains("--alert-color: #ffffff"), "a bad colour falls back: {css}");
        assert!(css.contains("--alert-background: rgba(51, 102, 153, 0.85)"), "{css}");
        assert!(css.contains("--alert-size: 8px"), "{css}");
        assert!(css.contains("--alert-style: normal"), "{css}");
        assert!(AlertStyle { italic: true, ..style }.css().contains("--alert-style: italic"));
        assert!(!css.contains('}') && !css.contains("url("), "{css}");
    }

    #[test]
    fn fonts_and_pictures() {
        let style = |font: &str, picture: &str| AlertStyle {
            font: font.into(),
            picture: picture.into(),
            ..AlertStyle::default()
        };
        assert_eq!(style("", "").font_stylesheet(), None);
        assert_eq!(style("monospace", "").font_stylesheet(), None);
        assert!(style("monospace", "").css().contains("--alert-font: monospace;"));
        assert_eq!(
            style("Press Start 2P", "").font_stylesheet().as_deref(),
            Some("https://fonts.googleapis.com/css2?family=Press+Start+2P&display=swap")
        );
        assert_eq!(style("", "").picture_url(), None);
        assert_eq!(style("", "big hype.gif").picture_url().as_deref(), Some("/pictures/big%20hype%2Egif"));
        assert_eq!(style("", "https://x.test/a.gif").picture_url().as_deref(), Some("https://x.test/a.gif"));
        assert_eq!(parse_hex("#fA0"), Some((255, 170, 0)));
        assert_eq!(parse_hex("#12345"), None);
    }
}
