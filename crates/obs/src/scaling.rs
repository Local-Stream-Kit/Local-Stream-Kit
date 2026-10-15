//! Browser Sources showing the app that OBS scales, and what unscales
//! them. A Browser Source draws its page at its Width × Height, then OBS
//! stretches that to the item's size in the scene, which blurs it; the
//! same size on screen is sharp with the page at that size and the item at
//! scale 1.

use serde::Deserialize;
use serde_json::{Value, json};

/// A scale this close to 1 is 1 (OBS keeps scales as 32-bit floats).
const SAME: f64 = 1e-4;

/// No bounding box: the item's scale decides its size.
const NO_BOUNDS: &str = "OBS_BOUNDS_NONE";

/// What decides a scene item's size on screen, from its
/// `sceneItemTransform` (obs-websocket 5).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub scale_x: f64,
    pub scale_y: f64,
    /// The source's own size: a Browser Source's page.
    pub source_width: f64,
    pub source_height: f64,
    #[serde(default = "no_bounds")]
    pub bounds_type: String,
    #[serde(default)]
    pub bounds_width: f64,
    #[serde(default)]
    pub bounds_height: f64,
    #[serde(default)]
    pub crop_left: f64,
    #[serde(default)]
    pub crop_right: f64,
    #[serde(default)]
    pub crop_top: f64,
    #[serde(default)]
    pub crop_bottom: f64,
}

fn no_bounds() -> String {
    NO_BOUNDS.into()
}

/// A size in pixels, across and down.
pub type Size = (u32, u32);

/// Why an item's unscaled size can't be worked out: a bounding box with a
/// crop (OBS scales the cropped page to fit the box, and the crop would
/// have to be worked back).
#[derive(Debug, PartialEq)]
pub struct BoundsAndCrop;

impl Transform {
    pub fn read(value: &Value) -> Result<Transform, String> {
        Transform::deserialize(value).map_err(|error| format!("sceneItemTransform: {error}"))
    }

    fn bounded(&self) -> bool {
        self.bounds_type != NO_BOUNDS
    }

    fn cropped(&self) -> bool {
        [self.crop_left, self.crop_right, self.crop_top, self.crop_bottom].iter().any(|crop| *crop != 0.0)
    }

    /// The page's size.
    pub fn page(&self) -> Size {
        (pixels(self.source_width), pixels(self.source_height))
    }

    /// How much OBS stretches the page across and down (shrinks, under 1),
    /// as libobs works it out for each kind of bounding box.
    pub fn shown_scale(&self) -> (f64, f64) {
        if !self.bounded() {
            return (self.scale_x.abs(), self.scale_y.abs());
        }
        let width = (self.source_width - self.crop_left - self.crop_right).max(1.0);
        let height = (self.source_height - self.crop_top - self.crop_bottom).max(1.0);
        let (across, down) = (self.bounds_width / width, self.bounds_height / height);
        match self.bounds_type.as_str() {
            "OBS_BOUNDS_STRETCH" => (across, down),
            "OBS_BOUNDS_SCALE_OUTER" => (across.max(down), across.max(down)),
            "OBS_BOUNDS_SCALE_TO_WIDTH" => (across, across),
            "OBS_BOUNDS_SCALE_TO_HEIGHT" => (down, down),
            "OBS_BOUNDS_MAX_ONLY" if width <= self.bounds_width && height <= self.bounds_height => (1.0, 1.0),
            // OBS_BOUNDS_SCALE_INNER, and OBS_BOUNDS_MAX_ONLY when too big.
            _ => (across.min(down), across.min(down)),
        }
    }

    /// Whether OBS draws the page at another size than its own.
    pub fn is_scaled(&self) -> bool {
        let (across, down) = self.shown_scale();
        (across - 1.0).abs() > SAME || (down - 1.0).abs() > SAME
    }

    /// The page size that shows it as big as it is now without scaling:
    /// the size it's shown at, or its bounding box's.
    pub fn unscaled_page(&self) -> Result<Size, BoundsAndCrop> {
        if !self.is_scaled() {
            Ok(self.page())
        } else if self.bounded() && self.cropped() {
            Err(BoundsAndCrop)
        } else if self.bounded() {
            Ok((pixels(self.bounds_width), pixels(self.bounds_height)))
        } else {
            Ok((
                pixels(self.source_width * self.scale_x.abs()),
                pixels(self.source_height * self.scale_y.abs()),
            ))
        }
    }

    /// The same size settings (scale, bounding box, crop) as `other`, as
    /// far as OBS keeps them: a move, a rotation or a new page size isn't a
    /// resize.
    pub fn sized_as(&self, other: &Transform) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= SAME * a.abs().max(b.abs()).max(1.0);
        self.bounds_type == other.bounds_type
            && near(self.scale_x, other.scale_x)
            && near(self.scale_y, other.scale_y)
            && near(self.bounds_width, other.bounds_width)
            && near(self.bounds_height, other.bounds_height)
            && near(self.crop_left, other.crop_left)
            && near(self.crop_right, other.crop_right)
            && near(self.crop_top, other.crop_top)
            && near(self.crop_bottom, other.crop_bottom)
    }

    /// The change (for SetSceneItemTransform) that shows it unscaled once
    /// its page is [`unscaled_page`](Self::unscaled_page): scale 1 (-1 when
    /// flipped), no bounding box, the crop as big as it's shown.
    pub fn unscaled(&self) -> Value {
        let mut change = json!({ "scaleX": sign(self.scale_x), "scaleY": sign(self.scale_y) });
        if self.bounded() {
            change["boundsType"] = NO_BOUNDS.into();
        } else if self.cropped() {
            let (across, down) = self.shown_scale();
            crop(&mut change, self, across, down);
        }
        change
    }

    /// The change that keeps it looking as it does when its page goes from
    /// its size to `page` (it's then scaled, unless it was as much as the
    /// page grows). `None` with a bounding box: OBS fits the new page to
    /// the box, so it can't be kept.
    pub fn kept(&self, page: Size) -> Option<Value> {
        if self.bounded() {
            return None;
        }
        let across = self.source_width / f64::from(page.0);
        let down = self.source_height / f64::from(page.1);
        let mut change = json!({ "scaleX": self.scale_x * across, "scaleY": self.scale_y * down });
        if self.cropped() {
            crop(&mut change, self, 1.0 / across, 1.0 / down);
        }
        Some(change)
    }
}

/// Its crop, `across` and `down` times as big, into `change`.
fn crop(change: &mut Value, transform: &Transform, across: f64, down: f64) {
    change["cropLeft"] = (transform.crop_left * across).round().into();
    change["cropRight"] = (transform.crop_right * across).round().into();
    change["cropTop"] = (transform.crop_top * down).round().into();
    change["cropBottom"] = (transform.crop_bottom * down).round().into();
}

fn sign(scale: f64) -> f64 {
    if scale < 0.0 { -1.0 } else { 1.0 }
}

/// Whole pixels, at least 1.
fn pixels(length: f64) -> u32 {
    length.round().max(1.0) as u32
}

/// A source showing the app in a scene.
pub struct Shown<'a> {
    pub scene: &'a str,
    pub source: &'a str,
    pub transform: &'a Transform,
}

/// "500 × 700".
pub fn size((width, height): Size) -> String {
    format!("{width} × {height}")
}

/// The plugin's note under Scaled overlays: a line for each scaled one,
/// saying where it is and what to do (`warn_only`: Scaled overlays is
/// Warn, so nothing is fixed). Short: the panel is a narrow dock.
pub fn blurry(shown: &[Shown], warn_only: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for at in shown.iter().filter(|at| at.transform.is_scaled()) {
        let named = format!("“{}” in “{}”", at.source, at.scene);
        let page = size(at.transform.page());
        let Ok(unscaled) = at.transform.unscaled_page() else {
            lines.push(format!("{named}: bounding box and crop. Reset Transform and size it again."));
            continue;
        };
        let elsewhere = shown.iter().find(|other| {
            other.source == at.source
                && other.scene != at.scene
                && other.transform.unscaled_page() != Ok(unscaled)
        });
        let shown_at = format!("{named}: {page}, shown at {}", size(unscaled));
        let line = match elsewhere {
            Some(other) => format!(
                "{shown_at} ({} in “{}”). Needs its own Browser Source.",
                other.transform.unscaled_page().map_or_else(|_| "another size".to_string(), size),
                other.scene
            ),
            None if warn_only => {
                format!("{shown_at}. Set Width and Height to {}, then Reset Transform.", size(unscaled))
            }
            None => format!("{shown_at}. Fixed once left alone."),
        };
        lines.push(line);
    }
    lines
}

/// A transform as OBS will have it after `change` (what SetSceneItemTransform
/// sends): the change's fields over the transform's.
pub fn merge(transform: &mut Value, change: &Value) {
    if let (Some(transform), Some(change)) = (transform.as_object_mut(), change.as_object()) {
        for (key, value) in change {
            transform.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transform with no bounding box or crop.
    fn scaled(page: Size, scale: (f64, f64)) -> Transform {
        Transform {
            scale_x: scale.0,
            scale_y: scale.1,
            source_width: f64::from(page.0),
            source_height: f64::from(page.1),
            bounds_type: NO_BOUNDS.into(),
            bounds_width: 0.0,
            bounds_height: 0.0,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }

    fn bounded(page: Size, kind: &str, bounds: Size) -> Transform {
        Transform {
            bounds_type: kind.into(),
            bounds_width: f64::from(bounds.0),
            bounds_height: f64::from(bounds.1),
            ..scaled(page, (1.0, 1.0))
        }
    }

    #[test]
    fn as_obs_lists_them() {
        // The user's scene (fixtures/get_scene_item_list.json): the default
        // overlay stretched from 1920 × 1080 to a 2560 × 1440 canvas, the
        // chat unscaled.
        let answer: Value =
            serde_json::from_str(include_str!("../fixtures/get_scene_item_list.json")).unwrap();
        let items = answer["d"]["responseData"]["sceneItems"].as_array().unwrap();
        let transform = |name: &str| {
            let item = items.iter().find(|item| item["sourceName"] == name).unwrap();
            Transform::read(&item["sceneItemTransform"]).unwrap()
        };
        let overlay = transform("LSK Default");
        assert!(overlay.is_scaled());
        assert_eq!(overlay.page(), (1920, 1080));
        assert_eq!(overlay.unscaled_page(), Ok((2560, 1440)));
        assert_eq!(overlay.unscaled(), json!({ "scaleX": 1.0, "scaleY": 1.0 }));
        let chat = transform("Chat");
        assert!(!chat.is_scaled());
        assert_eq!(chat.unscaled_page(), Ok((500, 700)));
        // A bounding box the size of the screen it shows: not scaled.
        assert!(!transform("Second screen").is_scaled());
    }

    #[test]
    fn scaled_unevenly_or_flipped() {
        let chat = scaled((500, 700), (0.9, 1.3));
        assert_eq!(chat.unscaled_page(), Ok((450, 910)));
        let flipped = scaled((500, 700), (-1.2, 1.2));
        assert!(flipped.is_scaled());
        assert_eq!(flipped.unscaled_page(), Ok((600, 840)));
        assert_eq!(flipped.unscaled(), json!({ "scaleX": -1.0, "scaleY": 1.0 }));
        assert!(!scaled((500, 700), (-1.0, 1.00001)).is_scaled(), "flipped is sharp");
    }

    #[test]
    fn cropped_crops_as_much_of_the_bigger_page() {
        let chat = Transform { crop_top: 100.0, crop_left: 10.0, ..scaled((500, 700), (2.0, 1.5)) };
        assert_eq!(chat.unscaled_page(), Ok((1000, 1050)));
        assert_eq!(
            chat.unscaled(),
            json!({ "scaleX": 1.0, "scaleY": 1.0, "cropLeft": 20.0, "cropRight": 0.0, "cropTop": 150.0, "cropBottom": 0.0 })
        );
    }

    #[test]
    fn bounding_boxes() {
        let stretched = bounded((500, 700), "OBS_BOUNDS_STRETCH", (400, 900));
        assert_eq!(stretched.shown_scale(), (0.8, 900.0 / 700.0));
        assert_eq!(stretched.unscaled_page(), Ok((400, 900)));
        assert_eq!(stretched.unscaled(), json!({ "scaleX": 1.0, "scaleY": 1.0, "boundsType": NO_BOUNDS }));
        // Fitted inside: the page becomes the box.
        let inner = bounded((500, 700), "OBS_BOUNDS_SCALE_INNER", (1000, 1000));
        assert_eq!(inner.shown_scale(), (1000.0 / 700.0, 1000.0 / 700.0));
        assert_eq!(inner.unscaled_page(), Ok((1000, 1000)));
        assert_eq!(bounded((500, 700), "OBS_BOUNDS_SCALE_OUTER", (1000, 1000)).shown_scale(), (2.0, 2.0));
        assert_eq!(bounded((500, 700), "OBS_BOUNDS_SCALE_TO_WIDTH", (250, 10)).shown_scale(), (0.5, 0.5));
        assert_eq!(bounded((500, 700), "OBS_BOUNDS_SCALE_TO_HEIGHT", (10, 1400)).shown_scale(), (2.0, 2.0));
        // Only shrunk, when too big.
        assert!(!bounded((500, 700), "OBS_BOUNDS_MAX_ONLY", (1000, 1000)).is_scaled());
        assert_eq!(bounded((500, 700), "OBS_BOUNDS_MAX_ONLY", (250, 1000)).shown_scale(), (0.5, 0.5));
        // Its bounding box's size, page and all: sharp.
        assert!(!bounded((500, 700), "OBS_BOUNDS_SCALE_INNER", (500, 700)).is_scaled());
        let cropped = Transform { crop_top: 10.0, ..stretched };
        assert_eq!(cropped.unscaled_page(), Err(BoundsAndCrop));
    }

    #[test]
    fn kept_as_it_looks_on_another_page() {
        // The page goes from 500 × 700 to 1000 × 1400: an unscaled item
        // elsewhere halves, one at twice the size becomes unscaled.
        let other = scaled((500, 700), (1.0, 1.0));
        assert_eq!(other.kept((1000, 1400)), Some(json!({ "scaleX": 0.5, "scaleY": 0.5 })));
        let twice = Transform { crop_bottom: 50.0, ..scaled((500, 700), (2.0, 2.0)) };
        assert_eq!(
            twice.kept((1000, 1400)),
            Some(
                json!({ "scaleX": 1.0, "scaleY": 1.0, "cropLeft": 0.0, "cropRight": 0.0, "cropTop": 0.0, "cropBottom": 100.0 })
            )
        );
        assert_eq!(bounded((500, 700), "OBS_BOUNDS_STRETCH", (400, 900)).kept((1000, 1400)), None);
    }

    #[test]
    fn a_resize_is_a_new_scale_bounding_box_or_crop() {
        let chat = scaled((500, 700), (1.0, 1.0));
        // OBS's 32-bit float of what was sent.
        assert!(chat.sized_as(&scaled((500, 700), (1.000_000_1, 1.0))));
        assert!(chat.sized_as(&scaled((900, 700), (1.0, 1.0))), "a new page size");
        assert!(!chat.sized_as(&scaled((500, 700), (1.2, 1.0))));
        assert!(!chat.sized_as(&Transform { crop_top: 1.0, ..chat.clone() }));
        assert!(!chat.sized_as(&bounded((500, 700), "OBS_BOUNDS_STRETCH", (500, 700))));
    }

    #[test]
    fn where_its_blurry() {
        let (sharp, twice) = (scaled((500, 700), (1.0, 1.0)), scaled((500, 700), (2.0, 2.0)));
        let shown = |scene, transform| Shown { scene, source: "Chat", transform };
        assert_eq!(blurry(&[shown("WoW", &sharp)], false), Vec::<String>::new());
        assert_eq!(
            blurry(&[shown("WoW", &twice)], true),
            [
                "“Chat” in “WoW”: 500 × 700, shown at 1000 × 1400. Set Width and Height to 1000 × 1400, then Reset \
              Transform."
            ]
        );
        assert_eq!(
            blurry(&[shown("WoW", &sharp), shown("Cooking", &twice)], false),
            [
                "“Chat” in “Cooking”: 500 × 700, shown at 1000 × 1400 (500 × 700 in “WoW”). Needs its own Browser \
              Source."
            ]
        );
        let cropped = Transform { crop_top: 1.0, ..bounded((500, 700), "OBS_BOUNDS_STRETCH", (400, 900)) };
        assert!(blurry(&[shown("WoW", &cropped)], false)[0].contains("bounding box and crop"));
    }

    #[test]
    fn merged() {
        let mut transform = json!({ "scaleX": 2.0, "scaleY": 2.0, "sourceWidth": 500.0 });
        merge(&mut transform, &json!({ "scaleX": 1.0, "scaleY": 1.0, "sourceWidth": 1000 }));
        assert_eq!(transform, json!({ "scaleX": 1.0, "scaleY": 1.0, "sourceWidth": 1000 }));
    }
}
