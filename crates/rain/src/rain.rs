//! Emote rain: emotes fall, bounce off each other and the floor, pile up,
//! and fade out. Physics is rapier2d; drawing is one canvas (or `<img>`
//! elements when `ImageDecoder` is missing).

use std::cell::RefCell;
use std::rc::Rc;

use rapier2d::prelude::*;
use serde_json::{Map, Value};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, HtmlImageElement};

use crate::emote_images::{Decoded, EmoteFrames, EmoteImages, State};
use crate::settings::RainSettings;

/// rapier is tuned for real-world sizes: simulate in metres, draw in pixels.
/// Using pixels as metres would make a 64 px emote a 64 m object that falls
/// in slow motion.
const PX_PER_METRE: f64 = 100.0;
/// Fixed physics step, decoupled from the frame rate.
const STEP_SECONDS: f32 = 1.0 / 60.0;
/// Catch-up cap: animation frames pause while the page is hidden, and the
/// first frame back mustn't try to simulate the whole gap. At most this much
/// time is simulated per frame; slower frame rates (a throttled page) still
/// run in real time down to 4 fps.
const MAX_FRAME_SECONDS: f32 = 0.25;
const MAX_STEPS_PER_FRAME: u32 = (MAX_FRAME_SECONDS / STEP_SECONDS) as u32 + 1;
const WALL_METRES: f32 = 1.0;
/// How far above the screen the side walls reach, so emotes spawned above the
/// top edge stay inside.
const WALL_ABOVE_METRES: f32 = 20.0;
/// Collision groups: emotes that pile up meet the floor, the walls and each
/// other; emotes that fall off the bottom meet only the walls and each other,
/// so they fall past the floor and past the pile.
const FLOOR: Group = Group::GROUP_1;
const WALLS: Group = Group::GROUP_2;
const PILING: Group = Group::GROUP_3;
const FALLING: Group = Group::GROUP_4;

fn groups(memberships: Group, filter: Group) -> InteractionGroups {
    InteractionGroups::new(memberships, filter, InteractionTestMode::And)
}

/// One emote to rain: its image's path on the app, and the zero-width
/// emotes drawn on top of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sprite {
    pub image: String,
    pub overlays: Vec<String>,
}

/// An emote on screen.
struct Falling {
    body: RigidBodyHandle,
    base: Rc<EmoteFrames>,
    /// Zero-width emotes drawn on top of the base.
    overlays: Vec<Rc<EmoteFrames>>,
    /// On-screen size in px.
    width: f64,
    height: f64,
    spawned_ms: f64,
    fade_from_ms: Option<f64>,
    /// From its rain's settings (a node's changes, or the plugin's).
    lifetime_ms: f64,
    fade_ms: f64,
    /// `<img>` fallback only: the base's element, then the overlays'.
    elements: Vec<HtmlImageElement>,
}

struct Pending {
    sprite: Sprite,
    /// The rain's own settings, if its node changed some.
    settings: Option<Rc<RainSettings>>,
    /// Unix ms; the emote is dropped if its image isn't ready by then.
    deadline_ms: f64,
}

pub struct Rain {
    layer: HtmlElement,
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    images: Rc<RefCell<EmoteImages>>,
    settings: RainSettings,
    world: PhysicsWorld,
    walls: Vec<RigidBodyHandle>,
    /// CSS px.
    size: (f64, f64),
    pixel_ratio: f64,
    falling: Vec<Falling>,
    pending: Vec<Pending>,
    last_frame_ms: Option<f64>,
    accumulator: f32,
}

impl Rain {
    pub fn new(layer: HtmlElement) -> Result<Rain, JsValue> {
        let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let canvas: HtmlCanvasElement = document.create_element("canvas")?.dyn_into()?;
        layer.append_child(&canvas)?;
        let context: CanvasRenderingContext2d =
            canvas.get_context("2d")?.ok_or("no 2d canvas context")?.dyn_into()?;
        let images = Rc::new(RefCell::new(EmoteImages::new()));
        if images.borrow().uses_dom() {
            canvas.set_hidden(true);
        }
        let mut world = PhysicsWorld::new();
        world.integration_parameters.dt = STEP_SECONDS;
        let mut rain = Rain {
            layer,
            canvas,
            context,
            images,
            settings: RainSettings::default(),
            world,
            walls: Vec::new(),
            size: (0.0, 0.0),
            pixel_ratio: 1.0,
            falling: Vec::new(),
            pending: Vec::new(),
            last_frame_ms: None,
            accumulator: 0.0,
        };
        rain.apply_settings();
        Ok(rain)
    }

    /// The plugin's settings (when the page loads, and when they change).
    pub fn configure(&mut self, settings: RainSettings) {
        self.settings = settings;
        self.apply_settings();
    }

    /// Emotes to rain once their images are ready; `changes`: the node's
    /// changes to the settings, for these emotes (only the per-emote ones
    /// count: size, bounciness, friction, gravity, shape, lifetime, fade).
    pub fn spawn(&mut self, sprites: Vec<Sprite>, changes: Option<Map<String, Value>>, issued_at_ms: f64) {
        let deadline_ms = issued_at_ms + self.settings.spawn_timeout_ms;
        let settings = changes.map(|changes| Rc::new(self.settings.with_changes(&changes)));
        for sprite in sprites {
            EmoteImages::request(&self.images, &sprite.image);
            for overlay in &sprite.overlays {
                EmoteImages::request(&self.images, overlay);
            }
            self.pending.push(Pending { sprite, settings: settings.clone(), deadline_ms });
        }
    }

    /// Every animation frame.
    pub fn frame(&mut self, now_ms: f64) {
        self.fit_to_window();
        self.spawn_ready(now_ms);
        self.step_physics(now_ms);
        self.age(now_ms);
        self.draw(now_ms);
    }

    fn apply_settings(&mut self) {
        // Screen y points down, so gravity is positive y.
        self.world.gravity = Vector::new(0.0, self.settings.gravity as f32);
        // The walls' inset depends on the emote size and overlap.
        if self.size != (0.0, 0.0) {
            self.rebuild_walls();
        }
    }

    /// An emote's physics shape is this fraction of its image, so piled-up
    /// emotes overlap.
    fn shape_scale(overlap: f64) -> f64 {
        1.0 - overlap.clamp(0.0, 0.9)
    }

    /// Keeps the canvas and the walls matched to the page size.
    fn fit_to_window(&mut self) {
        let Some(window) = web_sys::window() else { return };
        let width = window.inner_width().ok().and_then(|w| w.as_f64()).unwrap_or(0.0);
        let height = window.inner_height().ok().and_then(|h| h.as_f64()).unwrap_or(0.0);
        let ratio = window.device_pixel_ratio().max(1.0);
        if (width, height) == self.size && ratio == self.pixel_ratio {
            return;
        }
        self.size = (width, height);
        self.pixel_ratio = ratio;
        self.canvas.set_width((width * ratio).round() as u32);
        self.canvas.set_height((height * ratio).round() as u32);
        self.rebuild_walls();
    }

    fn rebuild_walls(&mut self) {
        for wall in self.walls.drain(..) {
            self.world.remove_body(wall);
        }
        // Images stick out past their (smaller) physics shapes, so move the
        // floor and side walls in by that much for a typical emote; otherwise
        // the bottom row would hang off the screen.
        let inset = (self.settings.emote_height_px * (1.0 - Self::shape_scale(self.settings.overlap))
            / 2.0
            / PX_PER_METRE) as f32;
        let width = (self.size.0 / PX_PER_METRE) as f32;
        let height = (self.size.1 / PX_PER_METRE) as f32;
        let half = WALL_METRES / 2.0;
        let tall = (height + WALL_ABOVE_METRES) / 2.0;
        let middle = (height - WALL_ABOVE_METRES) / 2.0;
        let walls = [
            (Vector::new(width / 2.0, height - inset + half), width / 2.0 + WALL_METRES, half, FLOOR),
            (Vector::new(inset - half, middle), half, tall, WALLS),
            (Vector::new(width - inset + half, middle), half, tall, WALLS),
        ];
        for (position, half_width, half_height, group) in walls {
            let (body, _) = self.world.insert(
                RigidBodyBuilder::fixed().translation(position),
                ColliderBuilder::cuboid(half_width, half_height)
                    .friction(self.settings.friction as f32)
                    .collision_groups(groups(group, Group::ALL)),
            );
            self.walls.push(body);
        }
        self.world.wake_up_all(true);
    }

    /// Spawns pending emotes whose images are ready; drops ones that failed
    /// or missed their deadline.
    fn spawn_ready(&mut self, now_ms: f64) {
        let unix_now = js_sys::Date::now();
        let mut ready = Vec::new();
        {
            let images = self.images.borrow();
            self.pending.retain(|pending| {
                let base = match images.state(&pending.sprite.image) {
                    Some(State::Ready(frames)) => Rc::clone(frames),
                    Some(State::Loading) if unix_now <= pending.deadline_ms => return true,
                    _ => return false, // failed, or too late
                };
                let overlay_states: Vec<_> =
                    pending.sprite.overlays.iter().map(|overlay| images.state(overlay)).collect();
                let still_loading = overlay_states.iter().any(|state| matches!(state, Some(State::Loading)));
                if still_loading && unix_now <= pending.deadline_ms {
                    return true;
                }
                // Overlays that failed (or are still loading at the deadline)
                // are left off.
                let overlays = overlay_states
                    .into_iter()
                    .filter_map(|state| match state {
                        Some(State::Ready(frames)) => Some(Rc::clone(frames)),
                        _ => None,
                    })
                    .collect();
                ready.push((base, overlays, pending.settings.clone()));
                false
            });
        }
        for (base, overlays, settings) in ready {
            if let Err(error) = self.add(base, overlays, settings, now_ms) {
                crate::log::warn(&format!("couldn't spawn an emote: {error:?}"));
            }
        }
    }

    fn add(
        &mut self,
        base: Rc<EmoteFrames>,
        overlays: Vec<Rc<EmoteFrames>>,
        settings: Option<Rc<RainSettings>>,
        now_ms: f64,
    ) -> Result<(), JsValue> {
        // This emote's settings; the limit and the world's gravity stay the
        // plugin's (gravity differs per emote through its scale).
        let own = settings.map_or_else(|| self.settings.clone(), |settings| (*settings).clone());
        let max = self.settings.max_concurrent.max(0.0) as usize;
        if max > 0 {
            let alive = self.falling.iter().filter(|emote| emote.fade_from_ms.is_none()).count();
            for emote in self
                .falling
                .iter_mut()
                .filter(|emote| emote.fade_from_ms.is_none())
                .take((alive + 1).saturating_sub(max))
            {
                emote.fade_from_ms = Some(now_ms);
            }
        }

        let random = js_sys::Math::random;
        let variation = 1.0 + (random() * 2.0 - 1.0) * own.size_variation;
        let height = own.emote_height_px * variation;
        let width = if base.height > 0.0 { height * base.width / base.height } else { height };
        let screen_width = self.size.0;
        let x = if screen_width > width {
            width / 2.0 + random() * (screen_width - width)
        } else {
            screen_width / 2.0
        };
        let y = -height;

        let metres = |px: f64| (px / PX_PER_METRE) as f32;
        let body = RigidBodyBuilder::dynamic()
            .translation(Vector::new(metres(x), metres(y)))
            .linvel(Vector::new((random() as f32 - 0.5) * 2.0, random() as f32 * 2.0))
            .angvel((random() as f32 - 0.5) * 4.0)
            .gravity_scale(if self.settings.gravity > 0.0 {
                (own.gravity / self.settings.gravity) as f32
            } else {
                1.0
            });
        let shape = Self::shape_scale(own.overlap);
        let (half_width, half_height) = (metres(width / 2.0 * shape), metres(height / 2.0 * shape));
        let radius = half_width.min(half_height) * own.corner_rounding.clamp(0.0, 1.0) as f32;
        // Mass and inertia of a plain box, with the centre of mass moved
        // towards the bottom edge (+y, since screen y points down).
        let uniform = MassProperties::from_cuboid(1.0, Vector::new(half_width, half_height));
        let centre_of_mass = Vector::new(0.0, half_height * own.bottom_weight.clamp(0.0, 1.0) as f32);
        let collider = ColliderBuilder::round_cuboid(half_width - radius, half_height - radius, radius)
            .mass_properties(MassProperties::new(centre_of_mass, uniform.mass(), uniform.principal_inertia()))
            .restitution(own.restitution as f32)
            .friction(own.friction as f32)
            .collision_groups(if own.fall_through {
                groups(FALLING, WALLS | FALLING)
            } else {
                groups(PILING, FLOOR | WALLS | PILING)
            });
        let (body, _) = self.world.insert(body, collider);

        let mut elements = Vec::new();
        for frames in std::iter::once(&base).chain(overlays.iter()) {
            if let Decoded::Element(template) = &frames.decoded {
                let element: HtmlImageElement = template.clone_node()?.dyn_into()?;
                self.layer.append_child(&element)?;
                elements.push(element);
            }
        }
        self.falling.push(Falling {
            body,
            base,
            overlays,
            width,
            height,
            spawned_ms: now_ms,
            fade_from_ms: None,
            lifetime_ms: own.lifetime_ms,
            fade_ms: own.fade_ms,
            elements,
        });
        Ok(())
    }

    fn step_physics(&mut self, now_ms: f64) {
        let elapsed = self.last_frame_ms.map(|last| ((now_ms - last) / 1000.0) as f32).unwrap_or(0.0);
        self.last_frame_ms = Some(now_ms);
        self.accumulator += elapsed.clamp(0.0, MAX_FRAME_SECONDS);
        let mut steps = 0;
        while self.accumulator >= STEP_SECONDS && steps < MAX_STEPS_PER_FRAME {
            self.world.step();
            self.accumulator -= STEP_SECONDS;
            steps += 1;
        }
        if steps == MAX_STEPS_PER_FRAME {
            self.accumulator = 0.0;
        }
    }

    /// Starts fading emotes past their lifetime; removes faded ones and ones
    /// that fell off the bottom.
    fn age(&mut self, now_ms: f64) {
        for emote in &mut self.falling {
            if emote.fade_from_ms.is_none() && now_ms - emote.spawned_ms >= emote.lifetime_ms {
                emote.fade_from_ms = Some(now_ms);
            }
        }
        let bottom = self.size.1;
        let world = &mut self.world;
        self.falling.retain(|emote| {
            let fell_off = bottom > 0.0
                && world.bodies.get(emote.body).is_some_and(|body| {
                    f64::from(body.translation().y) * PX_PER_METRE - emote.width.max(emote.height) > bottom
                });
            let done = fell_off || emote.fade_from_ms.is_some_and(|from| now_ms - from >= emote.fade_ms);
            if done {
                world.remove_body(emote.body);
                for element in &emote.elements {
                    element.remove();
                }
            }
            !done
        });
    }

    fn alpha(&self, emote: &Falling, now_ms: f64) -> f64 {
        match emote.fade_from_ms {
            Some(from) => (1.0 - (now_ms - from) / emote.fade_ms.max(1.0)).clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    fn draw(&self, now_ms: f64) {
        if self.images.borrow().uses_dom() {
            self.draw_elements(now_ms);
        } else {
            self.draw_canvas(now_ms);
        }
    }

    fn draw_canvas(&self, now_ms: f64) {
        let context = &self.context;
        let _ = context.set_transform(self.pixel_ratio, 0.0, 0.0, self.pixel_ratio, 0.0, 0.0);
        context.clear_rect(0.0, 0.0, self.size.0, self.size.1);
        for emote in &self.falling {
            let Some(body) = self.world.bodies.get(emote.body) else { continue };
            let position = body.translation();
            context.save();
            let _ =
                context.translate(f64::from(position.x) * PX_PER_METRE, f64::from(position.y) * PX_PER_METRE);
            let _ = context.rotate(f64::from(body.rotation().angle()));
            context.set_global_alpha(self.alpha(emote, now_ms));
            let elapsed = now_ms - emote.spawned_ms;
            if let Some(bitmap) = emote.base.frame_at(elapsed) {
                let _ = context.draw_image_with_image_bitmap_and_dw_and_dh(
                    bitmap,
                    -emote.width / 2.0,
                    -emote.height / 2.0,
                    emote.width,
                    emote.height,
                );
            }
            // Overlays: centred on the base, at the base's scale.
            let scale = if emote.base.height > 0.0 { emote.height / emote.base.height } else { 1.0 };
            for overlay in &emote.overlays {
                if let Some(bitmap) = overlay.frame_at(elapsed) {
                    let (width, height) = (overlay.width * scale, overlay.height * scale);
                    let _ = context.draw_image_with_image_bitmap_and_dw_and_dh(
                        bitmap,
                        -width / 2.0,
                        -height / 2.0,
                        width,
                        height,
                    );
                }
            }
            context.restore();
        }
    }

    fn draw_elements(&self, now_ms: f64) {
        for emote in &self.falling {
            let Some(body) = self.world.bodies.get(emote.body) else { continue };
            let position = body.translation();
            let (x, y) = (f64::from(position.x) * PX_PER_METRE, f64::from(position.y) * PX_PER_METRE);
            let angle = body.rotation().angle();
            let alpha = self.alpha(emote, now_ms);
            let scale = if emote.base.height > 0.0 { emote.height / emote.base.height } else { 1.0 };
            let frames = std::iter::once(&emote.base).chain(emote.overlays.iter());
            for (index, (element, frames)) in emote.elements.iter().zip(frames).enumerate() {
                let (width, height) = if index == 0 {
                    (emote.width, emote.height)
                } else {
                    (frames.width * scale, frames.height * scale)
                };
                let style = element.style();
                let _ = style.set_property("width", &format!("{width}px"));
                let _ = style.set_property("height", &format!("{height}px"));
                let _ = style.set_property("opacity", &alpha.to_string());
                let _ = style.set_property(
                    "transform",
                    &format!("translate({}px, {}px) rotate({angle}rad)", x - width / 2.0, y - height / 2.0),
                );
            }
        }
    }
}
