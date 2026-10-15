//! Text styles: named looks for parts of a text, kept once for the whole
//! app (`AppConfig::text_styles`, the Plugins tab's "Text styles" box). A
//! text typed into a node marks a part with a style's name, `[highlight]…[/]`;
//! the part then reaches pages with the style (`TextPart`'s `style`), which
//! they draw over their own look. Anything a style leaves empty is the text
//! around it, so one style works in a big alert and a small chat line.

use serde::{Deserialize, Serialize};

use crate::alert::parse_hex;

/// One named look. Every value is checked when it becomes CSS
/// ([`TextStyle::css`]), so a config value can't inject any.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// What a text marks: `[name]…[/]`. Letters, digits and `_`
    /// ([`TextStyle::valid_name`]), like a placeholder's.
    pub name: String,
    /// A font installed on this PC, or a Google Fonts family; empty = the
    /// text's own.
    pub font: String,
    /// The size, as a percentage of the text around it.
    pub size_percent: f32,
    /// None = as the text around it.
    pub bold: Option<bool>,
    /// None = as the text around it.
    pub italic: Option<bool>,
    /// `#rrggbb`; empty = the text's own.
    pub colour: String,
    /// The colour it had when its colour was unticked (`colour` emptied),
    /// so ticking it again brings that back. Never applied, and not sent
    /// with parts ([`TextStyle::for_parts`]).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kept_colour: String,
}

impl Default for TextStyle {
    /// Changes nothing.
    fn default() -> Self {
        TextStyle {
            name: String::new(),
            font: String::new(),
            size_percent: 100.0,
            bold: None,
            italic: None,
            colour: String::new(),
            kept_colour: String::new(),
        }
    }
}

/// Generic CSS font families: never quoted, never fetched.
const GENERIC_FONTS: [&str; 7] =
    ["system-ui", "sans-serif", "serif", "monospace", "cursive", "fantasy", "math"];
pub(crate) const FALLBACK_FONTS: &str = "system-ui, \"Segoe UI\", sans-serif";

/// Fonts the panel suggests: common Windows fonts, then popular Google
/// Fonts (as the Alerts style's font setting does).
pub const FONT_SUGGESTIONS: [&str; 18] = [
    "Arial",
    "Arial Black",
    "Comic Sans MS",
    "Consolas",
    "Georgia",
    "Impact",
    "Segoe UI",
    "Trebuchet MS",
    "Verdana",
    "Bangers",
    "Bebas Neue",
    "Lilita One",
    "Luckiest Guy",
    "Montserrat",
    "Permanent Marker",
    "Poppins",
    "Press Start 2P",
    "Russo One",
];

pub const MIN_SIZE_PERCENT: f32 = 25.0;
pub const MAX_SIZE_PERCENT: f32 = 400.0;

impl TextStyle {
    /// The styles a new config starts with, to show how marking works.
    pub fn starters() -> Vec<TextStyle> {
        vec![
            TextStyle {
                name: "highlight".into(),
                bold: Some(true),
                colour: "#ffcc00".into(),
                ..TextStyle::default()
            },
            TextStyle { name: "small".into(), size_percent: 70.0, ..TextStyle::default() },
        ]
    }

    /// The style as a part carries it to pages: what it applies, without
    /// the panel's kept colour.
    pub fn for_parts(&self) -> TextStyle {
        TextStyle { kept_colour: String::new(), ..self.clone() }
    }

    /// Whether `name` can be marked in a text: letters, digits and `_`.
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// The part's `style` attribute: what this style changes, nothing else.
    pub fn css(&self) -> String {
        let mut css = Vec::new();
        if let Some(family) = font_family(&self.font) {
            css.push(format!("font-family: {family}"));
        }
        let size = number(self.size_percent, MIN_SIZE_PERCENT, MAX_SIZE_PERCENT);
        if size != 100.0 {
            css.push(format!("font-size: {size}%"));
        }
        match self.bold {
            Some(true) => css.push("font-weight: 700".into()),
            Some(false) => css.push("font-weight: 400".into()),
            None => {}
        }
        match self.italic {
            Some(true) => css.push("font-style: italic".into()),
            Some(false) => css.push("font-style: normal".into()),
            None => {}
        }
        if let Some((r, g, b)) = parse_hex(&self.colour) {
            css.push(format!("color: #{r:02x}{g:02x}{b:02x}"));
        }
        css.join("; ")
    }

    /// The part's class, for stylesheets (the chat display's look):
    /// `text-style-highlight`.
    pub fn class(&self) -> String {
        let name: String = self.name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        format!("text-style-{name}")
    }

    /// The stylesheet that loads its font from Google Fonts, if it has one.
    pub fn font_stylesheet(&self) -> Option<String> {
        font_stylesheet(&self.font)
    }
}

/// The first style named `name`.
pub fn find<'a>(styles: &'a [TextStyle], name: &str) -> Option<&'a TextStyle> {
    styles.iter().find(|style| style.name == name)
}

/// A font as a CSS `font-family` value, with fallbacks; none when empty.
pub(crate) fn font_family(font: &str) -> Option<String> {
    let font = font_name(font);
    match font.as_str() {
        "" => None,
        generic if GENERIC_FONTS.contains(&generic) => Some(generic.to_string()),
        name => Some(format!("\"{name}\", {FALLBACK_FONTS}")),
    }
}

/// The stylesheet that loads a font from Google Fonts, unless it's a
/// generic family or none. (A font installed on the PC simply isn't found
/// there, and the local one is used.)
pub(crate) fn font_stylesheet(font: &str) -> Option<String> {
    let font = font_name(font);
    if font.is_empty() || GENERIC_FONTS.contains(&font.as_str()) {
        return None;
    }
    Some(format!("https://fonts.googleapis.com/css2?family={}&display=swap", font.replace(' ', "+")))
}

/// A font name without anything that could end the CSS value.
fn font_name(font: &str) -> String {
    font.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// A finite number within bounds.
pub(crate) fn number(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() { value.clamp(min, max) } else { min }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_has_only_what_the_style_changes() {
        assert_eq!(TextStyle::default().css(), "");
        let style = TextStyle {
            name: "big".into(),
            font: "Bangers".into(),
            size_percent: 150.0,
            bold: Some(false),
            italic: Some(true),
            colour: "#fA0".into(),
            kept_colour: String::new(),
        };
        assert_eq!(
            style.css(),
            "font-family: \"Bangers\", system-ui, \"Segoe UI\", sans-serif; font-size: 150%; \
             font-weight: 400; font-style: italic; color: #ffaa00"
        );
        assert_eq!(style.class(), "text-style-big");
        assert!(style.font_stylesheet().is_some_and(|sheet| sheet.ends_with("family=Bangers&display=swap")));
    }

    #[test]
    fn css_is_built_from_checked_values() {
        let style = TextStyle {
            font: "x\"; } body { display: none".into(),
            size_percent: f32::NAN,
            colour: "red; background: url(x)".into(),
            ..TextStyle::default()
        };
        let css = style.css();
        assert!(css.contains("font-family: \"x  body  display none\""), "{css}");
        assert!(!css.contains('}') && !css.contains("url("), "{css}");
        assert!(css.contains("font-size: 25%"), "{css}");
        assert!(!css.contains("color"), "a bad colour changes nothing: {css}");
    }

    #[test]
    fn a_kept_colour_is_only_kept() {
        let style = TextStyle { name: "big".into(), kept_colour: "#ffcc00".into(), ..TextStyle::default() };
        assert_eq!(style.css(), "", "not applied");
        let part = serde_json::to_value(style.for_parts()).unwrap();
        assert!(part.get("kept_colour").is_none(), "not sent with parts: {part}");
        let saved = serde_json::to_value(&style).unwrap();
        assert_eq!(saved["kept_colour"], "#ffcc00", "saved in the config");
        let plain = serde_json::to_value(TextStyle::default()).unwrap();
        assert!(plain.get("kept_colour").is_none(), "left out when there's none: {plain}");
    }

    #[test]
    fn names() {
        assert!(TextStyle::valid_name("highlight_2"));
        assert!(!TextStyle::valid_name(""));
        assert!(!TextStyle::valid_name("big text"));
        assert!(!TextStyle::valid_name("e:Kappa"));
        let styles = TextStyle::starters();
        assert_eq!(find(&styles, "small").map(|style| style.size_percent), Some(70.0));
        assert_eq!(find(&styles, "Small"), None);
    }
}
