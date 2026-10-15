//! Loads, decodes and caches emote images, by their path on the app.
//!
//! Animated emotes must animate, and `createImageBitmap` on an image (or
//! drawing an animated `<img>` to a canvas) only gives the first frame, so
//! every frame is decoded with WebCodecs `ImageDecoder` into an
//! `ImageBitmap`. If `ImageDecoder` is missing, images load as `<img>`
//! elements instead and modules draw them as DOM nodes, which the browser
//! animates by itself.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    HtmlImageElement, ImageBitmap, ImageDecodeOptions, ImageDecodeResult, ImageDecoder, ImageDecoderInit,
    Response, VideoFrame,
};

use crate::log;

/// Decoded frames are raw RGBA, so they add up; least recently used emotes
/// not on screen are dropped beyond this.
const MEMORY_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// Browsers play GIF frames this short at 100 ms; so do we.
const MIN_FRAME_MS: f64 = 10.0;
const DEFAULT_FRAME_MS: f64 = 100.0;

pub enum Decoded {
    /// Every frame, with its duration in ms.
    Frames { bitmaps: Vec<ImageBitmap>, durations_ms: Vec<f64>, total_ms: f64 },
    /// Fallback without `ImageDecoder`: an `<img>` the browser animates.
    Element(HtmlImageElement),
}

pub struct EmoteFrames {
    pub width: f64,
    pub height: f64,
    pub decoded: Decoded,
}

impl EmoteFrames {
    /// The frame to show `elapsed_ms` after an emote appeared.
    pub fn frame_at(&self, elapsed_ms: f64) -> Option<&ImageBitmap> {
        let Decoded::Frames { bitmaps, durations_ms, total_ms } = &self.decoded else { return None };
        if bitmaps.len() <= 1 || *total_ms <= 0.0 {
            return bitmaps.first();
        }
        let mut t = elapsed_ms.rem_euclid(*total_ms);
        for (bitmap, duration) in bitmaps.iter().zip(durations_ms) {
            if t < *duration {
                return Some(bitmap);
            }
            t -= duration;
        }
        bitmaps.last()
    }

    fn bytes(&self) -> usize {
        let frames = match &self.decoded {
            Decoded::Frames { bitmaps, .. } => bitmaps.len(),
            Decoded::Element(_) => 1,
        };
        (self.width * self.height * 4.0) as usize * frames
    }

    fn close(&self) {
        if let Decoded::Frames { bitmaps, .. } = &self.decoded {
            for bitmap in bitmaps {
                bitmap.close();
            }
        }
    }
}

pub enum State {
    Loading,
    Ready(Rc<EmoteFrames>),
    Failed,
}

pub struct EmoteImages {
    entries: HashMap<String, (State, u64)>,
    use_counter: u64,
    bytes: usize,
    use_decoder: bool,
}

impl EmoteImages {
    pub fn new() -> EmoteImages {
        let use_decoder = web_sys::window()
            .is_some_and(|window| Reflect::has(&window, &JsValue::from_str("ImageDecoder")).unwrap_or(false));
        if !use_decoder {
            log::warn("ImageDecoder isn't available; falling back to <img> elements for emotes");
        }
        EmoteImages { entries: HashMap::new(), use_counter: 0, bytes: 0, use_decoder }
    }

    pub fn uses_dom(&self) -> bool {
        !self.use_decoder
    }

    /// Starts loading an image if it isn't loaded or loading already.
    pub fn request(this: &Rc<RefCell<EmoteImages>>, image: &str) {
        let use_decoder = {
            let mut images = this.borrow_mut();
            images.use_counter += 1;
            let counter = images.use_counter;
            if let Some((_, last_used)) = images.entries.get_mut(image) {
                *last_used = counter;
                return;
            }
            images.entries.insert(image.to_string(), (State::Loading, counter));
            images.use_decoder
        };
        let (this, image) = (Rc::clone(this), image.to_string());
        wasm_bindgen_futures::spawn_local(async move {
            let result = if use_decoder { decode_frames(&image).await } else { load_element(&image).await };
            let mut images = this.borrow_mut();
            let state = match result {
                Ok(frames) => {
                    images.bytes += frames.bytes();
                    State::Ready(Rc::new(frames))
                }
                Err(error) => {
                    log::warn(&format!("couldn't load emote {image}: {error:?}"));
                    State::Failed
                }
            };
            if let Some((entry, _)) = images.entries.get_mut(&image) {
                *entry = state;
            }
            images.evict();
        });
    }

    pub fn state(&self, image: &str) -> Option<&State> {
        self.entries.get(image).map(|(state, _)| state)
    }

    /// Drops least recently used emotes until decoded frames fit the budget.
    /// Emotes still on screen (held by a sprite) are never dropped.
    fn evict(&mut self) {
        while self.bytes > MEMORY_BUDGET_BYTES {
            let candidate = self
                .entries
                .iter()
                .filter_map(|(image, (state, last_used))| match state {
                    State::Ready(frames) if Rc::strong_count(frames) == 1 => {
                        Some((image.clone(), *last_used))
                    }
                    _ => None,
                })
                .min_by_key(|(_, last_used)| *last_used)
                .map(|(image, _)| image);
            let Some(image) = candidate else { break };
            if let Some((State::Ready(frames), _)) = self.entries.remove(&image) {
                self.bytes = self.bytes.saturating_sub(frames.bytes());
                frames.close();
            }
        }
    }
}

async fn fetch_bytes(path: &str) -> Result<(Uint8Array, String), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let response: Response = JsFuture::from(window.fetch_with_str(path)).await?.dyn_into()?;
    if !response.ok() {
        return Err(format!("{path} returned {}", response.status()).into());
    }
    let mime = response.headers().get("content-type")?.unwrap_or_default();
    let buffer = JsFuture::from(response.array_buffer()?).await?;
    Ok((Uint8Array::new(&buffer), mime))
}

/// fetch → ImageDecoder → per frame: VideoFrame → ImageBitmap, then close the
/// VideoFrame (it holds decoder memory).
async fn decode_frames(path: &str) -> Result<EmoteFrames, JsValue> {
    let (bytes, mime) = fetch_bytes(path).await?;
    let decoder = ImageDecoder::new(&ImageDecoderInit::new(&bytes, &mime))?;
    let result = decode_all(&decoder).await;
    decoder.close();
    result
}

async fn decode_all(decoder: &ImageDecoder) -> Result<EmoteFrames, JsValue> {
    // Tracks are only known once `tracks.ready` resolves; the frame count is
    // only final once all the data is in (`completed`).
    let tracks = decoder.tracks();
    JsFuture::from(tracks.ready()).await?;
    JsFuture::from(decoder.completed()).await?;
    let track = tracks.selected_track().ok_or("image has no track")?;
    let window = web_sys::window().ok_or("no window")?;
    let mut bitmaps = Vec::new();
    let mut durations_ms = Vec::new();
    for index in 0..track.frame_count().max(1) {
        let options = ImageDecodeOptions::new();
        options.set_frame_index(index);
        let decoded: ImageDecodeResult =
            JsFuture::from(decoder.decode_with_options(&options)).await?.unchecked_into();
        let frame: VideoFrame = decoded.get_image();
        let duration_us = frame.duration();
        let bitmap = JsFuture::from(window.create_image_bitmap_with_video_frame(&frame)?).await;
        frame.close();
        let bitmap: ImageBitmap = bitmap?.unchecked_into();
        let duration_ms =
            duration_us.map(|us| us / 1000.0).filter(|ms| *ms > MIN_FRAME_MS).unwrap_or(DEFAULT_FRAME_MS);
        bitmaps.push(bitmap);
        durations_ms.push(duration_ms);
    }
    let first = bitmaps.first().ok_or("image has no frames")?;
    let (width, height) = (f64::from(first.width()), f64::from(first.height()));
    let total_ms = if bitmaps.len() > 1 { durations_ms.iter().sum() } else { 0.0 };
    Ok(EmoteFrames { width, height, decoded: Decoded::Frames { bitmaps, durations_ms, total_ms } })
}

async fn load_element(path: &str) -> Result<EmoteFrames, JsValue> {
    let image = HtmlImageElement::new()?;
    image.set_src(path);
    JsFuture::from(image.decode()).await?;
    let (width, height) = (f64::from(image.natural_width()), f64::from(image.natural_height()));
    Ok(EmoteFrames { width, height, decoded: Decoded::Element(image) })
}
