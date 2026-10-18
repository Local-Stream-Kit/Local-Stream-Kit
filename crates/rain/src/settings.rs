//! The Rain plugin's own settings (its manifest's `settings`, set on the
//! Effects page), as the page gets them: `{ "type": "settings", "values" }`.
//! A run's `changes` (a node's "Change a setting") apply over them for
//! that run's emotes. Numbers arrive as JSON numbers, so every number is
//! an `f64`; the manifest's bounds keep them sensible.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RainSettings {
    /// Most emotes on screen at once; 0 = unlimited. The oldest fade out first.
    pub max_concurrent: f64,
    /// How long an emote stays before it starts fading out.
    pub lifetime_ms: f64,
    /// How long the fade-out takes.
    pub fade_ms: f64,
    /// Drop an emote whose image isn't ready this long after the run was
    /// issued, instead of raining it long after the chat message.
    pub spawn_timeout_ms: f64,
    /// Typical emote height on screen, in pixels.
    pub emote_height_px: f64,
    /// Random size variation as a fraction (0.25 = ±25%).
    pub size_variation: f64,
    /// Bounciness, 0.0–1.0.
    pub restitution: f64,
    /// Friction between emotes, and with the floor and walls.
    pub friction: f64,
    /// Gravity in metres per second².
    pub gravity: f64,
    /// How much piled-up emotes overlap, as a fraction of their size: each
    /// emote's physics shape is this much smaller than its image. 0 = edges
    /// just touch; 0.25 = a quarter of each emote can sit under its
    /// neighbours.
    pub overlap: f64,
    /// Where an emote's weight sits, from its middle (0) to its bottom edge
    /// (1). Bottom-heavy emotes come to rest the right way up more often.
    pub bottom_weight: f64,
    /// How rounded the corners of an emote's physics shape are, as a fraction
    /// of its shorter half side: 0 = square, 1 = fully round. Rounded corners
    /// keep bottom-heavy emotes from resting on their sides.
    pub corner_rounding: f64,
    /// No floor: emotes fall past the bottom of the screen (and past the
    /// ones piled up there) and are gone, instead of piling up.
    pub fall_through: bool,
}

impl Default for RainSettings {
    /// The same as the manifest's defaults (`crates/server/plugins/rain`).
    fn default() -> Self {
        RainSettings {
            max_concurrent: 0.0,
            lifetime_ms: 8_000.0,
            fade_ms: 1_000.0,
            spawn_timeout_ms: 5_000.0,
            emote_height_px: 64.0,
            size_variation: 0.25,
            restitution: 0.35,
            friction: 0.6,
            gravity: 9.81,
            overlap: 0.25,
            bottom_weight: 0.8,
            corner_rounding: 0.4,
            fall_through: false,
        }
    }
}

impl RainSettings {
    /// From the plugin's settings message; what doesn't fit is the default.
    pub fn from_values(values: Map<String, Value>) -> RainSettings {
        serde_json::from_value(Value::Object(values)).unwrap_or_default()
    }

    /// These settings with a node's changes over them.
    pub fn with_changes(&self, changes: &Map<String, Value>) -> RainSettings {
        let Ok(Value::Object(mut values)) = serde_json::to_value(self) else { return self.clone() };
        for (key, value) in changes {
            values.insert(key.clone(), value.clone());
        }
        RainSettings::from_values(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_and_changes() {
        let values = json!({ "lifetime_ms": 4000.0, "gravity": 9.8, "unknown": 1 });
        let settings = RainSettings::from_values(values.as_object().unwrap().clone());
        assert_eq!(settings.lifetime_ms, 4000.0);
        assert_eq!(settings.fade_ms, 1000.0, "missing: the default");
        let changed =
            settings.with_changes(json!({ "restitution": 1.0, "fall_through": true }).as_object().unwrap());
        assert_eq!((changed.restitution, changed.lifetime_ms, changed.fall_through), (1.0, 4000.0, true));
    }
}
