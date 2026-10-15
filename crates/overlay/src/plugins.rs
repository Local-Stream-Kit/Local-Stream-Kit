//! Plugins' overlay pages: one sandboxed iframe each, above the built-in
//! layers (see `docs/writing-a-plugin.md` → Overlay pages). The shell
//! passes a plugin's commands on with `postMessage`; what's sent before its
//! page has loaded waits for it. A page says when a queued run is done
//! (`{ "type": "done", "queue_id" }`) and keeps a value with the app
//! (`{ "type": "keep", "value" }`), which go on to the server.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use protocol::ClientMessage;
use protocol::plugin::{PageMessage, PluginMessage, ShellMessage, page_path};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Event, HtmlIFrameElement, MessageEvent};

use crate::log;

struct Frame {
    element: HtmlIFrameElement,
    /// Messages waiting for the page to load; `None` once it has.
    waiting: Rc<RefCell<Option<Vec<JsValue>>>>,
    _on_load: Closure<dyn FnMut(Event)>,
}

pub struct PluginFrames {
    frames: BTreeMap<String, Frame>,
    /// The master volume and mute, for every page (a new one gets it
    /// first).
    volume: Option<ShellMessage>,
    /// Queued runs passed on and not done yet: their queue ids, with the
    /// plugin whose page may say they're done.
    queued: Rc<RefCell<BTreeMap<u64, String>>>,
    _on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
}

impl PluginFrames {
    /// Listens for pages saying a queued run is done.
    pub fn new() -> PluginFrames {
        let queued: Rc<RefCell<BTreeMap<u64, String>>> = Rc::default();
        let on_message = {
            let queued = Rc::clone(&queued);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let Some(json) =
                    js_sys::JSON::stringify(&event.data()).ok().and_then(|json| json.as_string())
                else {
                    return;
                };
                let queue_id = match serde_json::from_str::<PageMessage>(&json) {
                    Ok(PageMessage::Done { queue_id }) => queue_id,
                    Ok(PageMessage::Keep { value }) => {
                        if let Some(plugin) = sender(&event) {
                            crate::shell::send(&ClientMessage::Keep { plugin, value });
                        }
                        return;
                    }
                    Err(_) => return,
                };
                let mut queued = queued.borrow_mut();
                // Only the page the run went to can end it.
                let from_its_page = queued.get(&queue_id).is_some_and(|plugin| is_page(plugin, &event));
                if from_its_page {
                    queued.remove(&queue_id);
                    crate::shell::send(&ClientMessage::Done { id: queue_id });
                }
            })
        };
        let listening = web_sys::window().is_some_and(|window| {
            window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok()
        });
        if !listening {
            log::warn("couldn't listen to plugin pages; their queued effects won't say when they're done");
        }
        PluginFrames { frames: BTreeMap::new(), volume: None, queued, _on_message: Some(on_message) }
    }

    /// Shows exactly these plugins' pages: new ones get an iframe, the
    /// others' go.
    pub fn show(&mut self, plugins: &[String]) {
        self.frames.retain(|id, frame| {
            let keep = plugins.contains(id);
            if !keep {
                frame.element.remove();
            }
            keep
        });
        for id in plugins {
            if self.frames.contains_key(id) {
                continue;
            }
            match new_frame(id, self.volume.as_ref()) {
                Ok(frame) => {
                    self.frames.insert(id.clone(), frame);
                }
                Err(error) => log::warn(&format!("couldn't make the {id} plugin's layer: {error:?}")),
            }
        }
        if !self.frames.is_empty() {
            log::info(&format!("plugin pages: {:?}", self.frames.keys().collect::<Vec<_>>()));
        }
    }

    /// Passes a message on to a plugin's page, with the command's time.
    pub fn send(&mut self, plugin: &str, message: &PluginMessage, issued_at_ms: u64) {
        let Some(frame) = self.frames.get(plugin) else { return };
        match message {
            PluginMessage::Run { queue_id: Some(id), .. } => {
                let mut queued = self.queued.borrow_mut();
                // One is on stage at a time; older ones were skipped or
                // finished elsewhere.
                queued.clear();
                queued.insert(*id, plugin.to_string());
            }
            PluginMessage::Skip { queue_id } => {
                self.queued.borrow_mut().remove(queue_id);
            }
            _ => {}
        }
        let mut value = serde_json::to_value(message).expect("plugin messages serialise");
        if let Some(object) = value.as_object_mut() {
            object.insert("issued_at_ms".into(), issued_at_ms.into());
        }
        if let Some(message) = to_js(&value) {
            frame.pass(message);
        }
    }

    /// The master volume and mute changed: every page hears it.
    pub fn set_volume(&mut self, volume: f32, muted: bool) {
        let message = ShellMessage::Volume { volume, muted };
        if let Some(js) = to_js(&serde_json::to_value(&message).expect("shell messages serialise")) {
            for frame in self.frames.values() {
                frame.pass(js.clone());
            }
        }
        self.volume = Some(message);
    }
}

impl Frame {
    /// Posts a message, or keeps it until the page has loaded.
    fn pass(&self, message: JsValue) {
        match self.waiting.borrow_mut().as_mut() {
            Some(waiting) => waiting.push(message),
            None => post(&self.element, &message),
        }
    }
}

/// Whether a message came from this plugin's page.
/// The plugin whose page sent `event`, if a plugin page did.
fn sender(event: &MessageEvent) -> Option<String> {
    let source = JsValue::from(event.source()?);
    let frames = web_sys::window()?.document()?.query_selector_all("iframe.plugin").ok()?;
    (0..frames.length())
        .filter_map(|index| frames.item(index)?.dyn_into::<HtmlIFrameElement>().ok())
        .find(|frame| frame.content_window().is_some_and(|window| JsValue::from(window) == source))
        .and_then(|frame| frame.id().strip_prefix("plugin-").map(str::to_string))
}

fn is_page(plugin: &str, event: &MessageEvent) -> bool {
    let Some(source) = event.source() else { return false };
    let frame = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&format!("plugin-{plugin}")))
        .and_then(|element| element.dyn_into::<HtmlIFrameElement>().ok())
        .and_then(|frame| frame.content_window());
    frame.is_some_and(|window| JsValue::from(window) == JsValue::from(source))
}

fn to_js(value: &serde_json::Value) -> Option<JsValue> {
    js_sys::JSON::parse(&value.to_string()).ok()
}

fn post(element: &HtmlIFrameElement, message: &JsValue) {
    if let Some(window) = element.content_window() {
        // The page's origin is opaque (it's sandboxed), so it can't be named.
        if let Err(error) = window.post_message(message, "*") {
            log::warn(&format!("couldn't pass a message to a plugin page: {error:?}"));
        }
    }
}

fn new_frame(id: &str, volume: Option<&ShellMessage>) -> Result<Frame, JsValue> {
    let document = web_sys::window().and_then(|window| window.document()).ok_or("no document")?;
    let element: HtmlIFrameElement = document.create_element("iframe")?.dyn_into()?;
    element.set_class_name("layer plugin");
    element.set_id(&format!("plugin-{id}"));
    element.set_title(&format!("The {id} plugin"));
    // Its scripts run, but in an origin of its own: it can't reach this
    // page or the other plugins'.
    element.set_attribute("sandbox", "allow-scripts")?;
    element.set_attribute("allow", "autoplay")?;
    let first = volume.and_then(|volume| to_js(&serde_json::to_value(volume).ok()?));
    let waiting = Rc::new(RefCell::new(Some(first.into_iter().collect::<Vec<_>>())));
    let on_load = {
        let (waiting, element) = (Rc::clone(&waiting), element.clone());
        Closure::<dyn FnMut(Event)>::new(move |_: Event| {
            // Marked, for whoever wants to know it's ready (the e2e tests).
            let _ = element.set_attribute("data-loaded", "");
            for message in waiting.borrow_mut().take().unwrap_or_default() {
                post(&element, &message);
            }
        })
    };
    element.set_onload(Some(on_load.as_ref().unchecked_ref()));
    element.set_src(&page_path(id));
    document.body().ok_or("no body")?.append_child(&element)?;
    Ok(Frame { element, waiting, _on_load: on_load })
}
