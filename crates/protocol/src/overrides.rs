//! Overrides: a plugin node that reads a text aloud changing some of the
//! text to speech settings for itself (a voice that reads faster). (Its
//! plugin's own settings it changes with its type's `changes`.) A node
//! stores only what it changes, by the setting's JSON name (`{ "speed":
//! 1.5 }`); everything else comes from Plugins → Speak, and follows its
//! changes.
//!
//! The fields that can be overridden, with how the panel edits them, are
//! listed here once ([`SPEECH_FIELDS`]) for the server and the panel; and
//! the number [`Unit`]s plugin settings use too.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Changed settings by JSON name.
pub type Overrides = serde_json::Map<String, Value>;

/// How a number is shown and typed in the panel; stored as is. In a plugin
/// manifest: `plain`, `px`, `percent`, `ms`, `metres_per_second_squared`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    #[default]
    Plain,
    #[serde(rename = "px")]
    Pixels,
    /// Stored 0–1, shown 0–100 %.
    Percent,
    /// Stored in ms, shown in s.
    #[serde(rename = "ms")]
    Seconds,
    MetresPerSecondSquared,
}

impl Unit {
    pub fn is_plain(&self) -> bool {
        *self == Unit::Plain
    }

    /// Stored → shown.
    pub fn show(self, stored: f64) -> f64 {
        match self {
            Unit::Percent => stored * 100.0,
            Unit::Seconds => stored / 1000.0,
            _ => stored,
        }
    }

    /// Shown → stored.
    pub fn store(self, shown: f64) -> f64 {
        match self {
            Unit::Percent => shown / 100.0,
            Unit::Seconds => shown * 1000.0,
            _ => shown,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Plain => "",
            Unit::Pixels => "px",
            Unit::Percent => "%",
            Unit::Seconds => "s",
            Unit::MetresPerSecondSquared => "m/s²",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// Bounds as stored.
    Number {
        min: f64,
        max: f64,
        unit: Unit,
    },
    /// `#rrggbb`.
    Colour,
    Text,
    Bool,
    /// A file in the pictures folder.
    Picture,
    /// A file in the sounds folder.
    Sound,
    /// A text to speech voice (`speech::VOICES`).
    Voice,
    /// `(JSON value, label)` pairs.
    Choice(fn() -> Vec<(String, &'static str)>),
}

#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// The setting's JSON name.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
}

const fn number(key: &'static str, label: &'static str, min: f64, max: f64, unit: Unit) -> Field {
    Field { key, label, kind: FieldKind::Number { min, max, unit } }
}

const fn field(key: &'static str, label: &'static str, kind: FieldKind) -> Field {
    Field { key, label, kind }
}

/// What a node reading aloud can change of the speech settings.
pub const SPEECH_FIELDS: &[Field] = &[
    field("voice", "Voice", FieldKind::Voice),
    number("speed", "Speed", 0.5, 2.0, Unit::Plain),
    number("volume", "Volume", 0.0, 1.0, Unit::Percent),
];

pub fn field_of(fields: &'static [Field], key: &str) -> Option<&'static Field> {
    fields.iter().find(|field| field.key == key)
}

/// `base` with the `overrides` for `fields` applied. A value of the wrong
/// type is skipped and a number out of its field's bounds is clamped, so an
/// override can't break the effect; unknown keys are ignored.
pub fn apply<T: Serialize + DeserializeOwned + Clone>(
    base: &T,
    overrides: &Overrides,
    fields: &[Field],
) -> T {
    let Ok(Value::Object(mut settings)) = serde_json::to_value(base) else { return base.clone() };
    let mut result = base.clone();
    for field in fields {
        let Some(value) = overrides.get(field.key) else { continue };
        let value = match (field.kind, value.as_f64()) {
            (FieldKind::Number { min, max, .. }, Some(number)) if number.is_finite() => {
                let number = number.clamp(min, max);
                // Whole-number settings (milliseconds) take whole numbers.
                match settings.get(field.key) {
                    Some(stored) if stored.is_u64() => serde_json::json!(number.round() as u64),
                    _ => serde_json::json!(number),
                }
            }
            (FieldKind::Number { .. }, _) => continue,
            _ => value.clone(),
        };
        let previous = settings.insert(field.key.to_string(), value);
        match serde_json::from_value::<T>(Value::Object(settings.clone())) {
            Ok(applied) => result = applied,
            Err(_) => {
                // Skipped: put the base value back.
                if let Some(previous) = previous {
                    settings.insert(field.key.to_string(), previous);
                }
            }
        }
    }
    result
}

/// The value `fields`' `key` has in `base`: where a new override starts.
pub fn value_of<T: Serialize>(base: &T, key: &str) -> Option<Value> {
    serde_json::to_value(base).ok()?.get(key).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpeechSettings;

    #[test]
    fn overrides_change_only_what_they_name() {
        let mut faster = Overrides::new();
        faster.insert("speed".into(), serde_json::json!(1.5));
        faster.insert("volume".into(), serde_json::json!(7)); // clamped
        faster.insert("max_chars".into(), serde_json::json!(5)); // not overridable
        let speech = apply(&SpeechSettings::default(), &faster, SPEECH_FIELDS);
        assert_eq!((speech.speed, speech.volume), (1.5, 1.0));
        assert_eq!(speech.voice, SpeechSettings::default().voice);
        assert_eq!(speech.max_chars, SpeechSettings::default().max_chars);
        assert_eq!(
            value_of(&SpeechSettings::default(), "speed"),
            Some(serde_json::json!(SpeechSettings::default().speed))
        );
    }

    #[test]
    fn units() {
        assert!(matches!(field_of(SPEECH_FIELDS, "voice").unwrap().kind, FieldKind::Voice));
        assert_eq!(Unit::Seconds.show(1500.0), 1.5);
        assert_eq!(Unit::Percent.store(50.0), 0.5);
    }
}
