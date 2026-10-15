//! The built-in Chat display plugin's overlay page: chat messages on the
//! stream, looking however the streamer's CSS (the plugin's `look` file
//! setting) says. The overlay shell hosts it in a sandboxed iframe on a
//! Browser Source of its own (the Chat overlay, `?overlay=chat`) and passes
//! it the plugin's messages with `postMessage` (the types are
//! `protocol::plugin`'s).
//!
//! The page hears the chat: every message comes as a `ChatLine`, and it
//! leaves out what its settings say (commands, bots). Its Show in chat node
//! adds other lines. The markup is `protocol::chat`'s. A line leaves
//! (pushed out by the limit, past its lifetime, deleted by a moderator) by
//! getting `msg--leaving`; it's removed once that class's animation or
//! transition has run, or at once without one. The page keeps its recent
//! lines with the app, so they come back when the Browser Source reloads
//! (marked `msg--restored`).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use protocol::chat::{ChatLine, ChatMarks, LineSource, line_parts, samples};
use protocol::plugin::{PageMessage, PluginMessage, RunViewer, TextValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, HtmlElement, HtmlTemplateElement, MessageEvent};

/// A leaving animation that never says it ended is cut off after this.
const MAX_LEAVE_MS: f64 = 10_000.0;
/// What's kept changes at most this often.
const KEEP_EVERY_MS: f64 = 500.0;
/// The most lines kept, whatever the settings say.
const MAX_KEPT: usize = 200;

/// The plugin's settings (see its manifest).
#[derive(Debug, Clone, PartialEq)]
struct Settings {
    /// Most messages on screen; the oldest leave first.
    max_messages: u32,
    /// Messages leave after this long; 0 = they stay until pushed out.
    lifetime_secs: f64,
    /// Show viewers' messages starting with `!` (off: they're left out).
    show_commands: bool,
    /// Viewers whose messages aren't shown (login or name, any case).
    hidden_users: Vec<String>,
    /// A reply shows the message it answers.
    show_reply_to: bool,
    /// The stylesheet.
    look: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_messages: 30,
            lifetime_secs: 0.0,
            show_commands: false,
            hidden_users: Vec::new(),
            show_reply_to: true,
            look: String::new(),
        }
    }
}

impl Settings {
    fn from_values(values: &Map<String, Value>) -> Settings {
        let default = Settings::default();
        let number = |key: &str| values.get(key).and_then(Value::as_f64);
        let flag = |key: &str, default: bool| values.get(key).and_then(Value::as_bool).unwrap_or(default);
        Settings {
            max_messages: number("max_messages")
                .map_or(default.max_messages, |max| max.clamp(1.0, 200.0) as u32),
            lifetime_secs: number("lifetime_secs").map_or(default.lifetime_secs, |secs| secs.max(0.0)),
            show_commands: flag("show_commands", default.show_commands),
            hidden_users: names(values.get("hidden_users")),
            show_reply_to: flag("show_reply_to", default.show_reply_to),
            look: values.get("look").and_then(Value::as_str).unwrap_or_default().to_string(),
        }
    }

    /// Whether a viewer's message stays off the display.
    fn leaves_out(&self, line: &ChatLine) -> bool {
        if line.source != LineSource::Chat {
            return false;
        }
        let command = match line.parts.first() {
            Some(protocol::AlertPart::Text { text, .. }) => text.trim_start().starts_with('!'),
            _ => false,
        };
        let hidden = line.user.as_ref().is_some_and(|user| {
            self.hidden_users.iter().any(|hidden| {
                hidden.eq_ignore_ascii_case(&user.name) || hidden.eq_ignore_ascii_case(&user.display_name)
            })
        });
        (!self.show_commands && command) || hidden
    }
}

/// A list of names, a line each (or, as before 2026-10-14, separated by
/// commas).
fn names(value: Option<&Value>) -> Vec<String> {
    let names: Vec<&str> = match value {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(text)) => text.split(',').collect(),
        _ => Vec::new(),
    };
    names.into_iter().map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect()
}

/// What the page keeps with the app.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    lines: Vec<ChatLine>,
}

/// The values a Show in chat node's run brings (see its manifest).
#[derive(Debug, Default, Deserialize)]
struct ShowValues {
    #[serde(default)]
    message: Option<TextValue>,
    #[serde(default)]
    class: Option<TextValue>,
    #[serde(default)]
    show_viewer: Option<bool>,
    #[serde(default)]
    viewer: Option<RunViewer>,
}

struct Line {
    line: ChatLine,
    element: Element,
    /// Leaving: removed at this time (`Date.now()`).
    remove_at_ms: Option<f64>,
    /// A sample (the preview's): not kept.
    sample: bool,
}

struct Page {
    root: HtmlElement,
    look: Element,
    settings: Settings,
    /// On the page, oldest first.
    lines: Vec<Line>,
    /// Numbers card lines.
    next_card: u64,
    /// What's kept changed since it was last sent.
    unkept: bool,
    kept_at_ms: f64,
    /// Google Fonts stylesheets loaded for text styles' fonts.
    fonts: HashSet<String>,
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let document = document()?;
    let root: HtmlElement = document.get_element_by_id("chat").ok_or("no #chat")?.dyn_into()?;
    let look = document.get_element_by_id("look").ok_or("no #look")?;
    let page = Rc::new(RefCell::new(Page {
        root,
        look,
        settings: Settings::default(),
        lines: Vec::new(),
        next_card: 1,
        unkept: false,
        kept_at_ms: 0.0,
        fonts: HashSet::new(),
    }));
    listen(Rc::clone(&page))?;
    start_animation_loop(page);
    Ok(())
}

/// Messages from the overlay shell (only from it: other pages could post
/// here too), first the ones that came before this started (`index.html`
/// kept them).
fn listen(page: Rc<RefCell<Page>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let parent = window.parent()?.map(JsValue::from);
    let handle = move |event: MessageEvent| {
        if event.source().map(JsValue::from) != parent {
            return;
        }
        let Some(json) = js_sys::JSON::stringify(&event.data()).ok().and_then(|json| json.as_string()) else {
            return;
        };
        let Ok(message) = serde_json::from_str::<PluginMessage>(&json) else { return };
        page.borrow_mut().handle(message);
    };
    let early = js_sys::Reflect::get(&window, &"chatEarly".into())?;
    if let Some(early) = early.dyn_ref::<js_sys::Function>() {
        window.remove_event_listener_with_callback("message", early)?;
    }
    let waiting = js_sys::Reflect::get(&window, &"chatWaiting".into())?;
    let waiting: Vec<MessageEvent> = waiting
        .dyn_into::<js_sys::Array>()
        .map(|waiting| waiting.iter().filter_map(|event| event.dyn_into().ok()).collect())
        .unwrap_or_default();
    let handle = Rc::new(handle);
    let on_message = {
        let handle = Rc::clone(&handle);
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| handle(event))
    };
    window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())?;
    // Listens for as long as the page is open.
    on_message.forget();
    for event in waiting {
        handle(event);
    }
    Ok(())
}

impl Page {
    fn handle(&mut self, message: PluginMessage) {
        let now_ms = js_sys::Date::now();
        let result = match message {
            PluginMessage::Settings { values } => {
                self.settings = Settings::from_values(&values);
                self.look.set_text_content(Some(&self.settings.look));
                self.trim(now_ms);
                // The preview's samples follow (a command shows once
                // Commands is ticked).
                if self.lines.iter().any(|line| line.sample) { self.samples() } else { Ok(()) }
            }
            PluginMessage::Kept { value } => {
                let kept: Kept = serde_json::from_value(value).unwrap_or_default();
                let lifetime_ms = self.settings.lifetime_secs * 1000.0;
                kept.lines
                    .into_iter()
                    .filter(|line| lifetime_ms == 0.0 || now_ms - (line.at_ms as f64) < lifetime_ms)
                    .try_for_each(|line| self.add(line, true, false))
            }
            PluginMessage::Chat { mut line } => {
                if self.settings.leaves_out(&line) {
                    return;
                }
                if !self.settings.show_reply_to {
                    line.marks.reply_to = None;
                }
                self.add(line, false, false)
            }
            PluginMessage::Run { values, .. } => match self.card_line(values, now_ms) {
                Some(line) => self.add(line, false, false),
                None => Ok(()),
            },
            PluginMessage::ChatRemoved { id } => {
                self.leave_where(|line| line.id == id);
                Ok(())
            }
            PluginMessage::ChatUserRemoved { platform, user_id } => {
                self.leave_where(|line| {
                    line.platform == platform && line.user.as_ref().is_some_and(|user| user.id == user_id)
                });
                Ok(())
            }
            PluginMessage::ChatCleared { platform } => {
                self.leave_where(|line| platform.is_empty() || line.platform == platform);
                Ok(())
            }
            PluginMessage::Samples => self.samples(),
            PluginMessage::Skip { .. } | PluginMessage::Message { .. } => Ok(()),
        };
        if let Err(error) = result {
            warn(&format!("couldn't show a chat line: {error:?}"));
        }
    }

    /// A Show in chat node's line: its message, with its class and (unless
    /// it's off) the viewer who set it off as its name.
    fn card_line(&mut self, values: Map<String, Value>, now_ms: f64) -> Option<ChatLine> {
        let values: ShowValues = serde_json::from_value(Value::Object(values)).unwrap_or_default();
        let message = values.message?;
        if message.text.trim().is_empty() {
            return None;
        }
        let viewer = values.viewer.filter(|_| values.show_viewer.unwrap_or(true));
        let id = format!("card:{now_ms}-{}", self.next_card);
        self.next_card += 1;
        Some(ChatLine {
            id,
            platform: viewer.as_ref().map(|viewer| viewer.platform.clone()).unwrap_or_default(),
            at_ms: now_ms as u64,
            source: LineSource::Card { class: values.class.map(|class| class.text).unwrap_or_default() },
            user: viewer.map(|viewer| viewer.user),
            parts: line_parts(&message.parts),
            marks: ChatMarks::default(),
        })
    }

    /// Sample lines of every kind the settings show (the preview's), in
    /// again: the ones shown before go at once.
    fn samples(&mut self) -> Result<(), JsValue> {
        self.lines.retain(|line| {
            if line.sample {
                line.element.remove();
            }
            !line.sample
        });
        let shown: Vec<ChatLine> =
            samples().into_iter().filter(|line| !self.settings.leaves_out(line)).collect();
        for mut line in shown {
            line.marks.reply |= line.marks.reply_to.is_some();
            if !self.settings.show_reply_to {
                line.marks.reply_to = None;
            }
            line.at_ms = js_sys::Date::now() as u64;
            self.add(line, false, true)?;
        }
        Ok(())
    }

    fn add(&mut self, line: ChatLine, restored: bool, sample: bool) -> Result<(), JsValue> {
        if self.lines.iter().any(|shown| shown.line.id == line.id) {
            return Ok(());
        }
        let document = document()?;
        // The fonts of the text styles a card's line has parts in.
        for sheet in line.parts.iter().filter_map(|part| part.style()?.font_stylesheet()) {
            if self.fonts.insert(sheet.clone()) {
                let link = document.create_element("link")?;
                link.set_attribute("rel", "stylesheet")?;
                link.set_attribute("href", &sheet)?;
                document.head().ok_or("no head")?.append_child(&link)?;
            }
        }
        let template: HtmlTemplateElement = document.create_element("template")?.dyn_into()?;
        template.set_inner_html(&line.html());
        let element = template.content().first_element_child().ok_or("a chat line without markup")?;
        if restored {
            element.class_list().add_1("msg--restored")?;
        }
        self.root.append_child(&element)?;
        self.lines.push(Line { line, element, remove_at_ms: None, sample });
        self.unkept |= !sample;
        self.trim(js_sys::Date::now());
        Ok(())
    }

    /// Lines over the limit, and past their lifetime, leave.
    fn trim(&mut self, now_ms: f64) {
        let lifetime_ms = self.settings.lifetime_secs * 1000.0;
        let max = self.settings.max_messages.max(1) as usize;
        let staying = self.lines.iter().filter(|line| line.remove_at_ms.is_none()).count();
        let mut over = staying.saturating_sub(max);
        for line in &mut self.lines {
            if line.remove_at_ms.is_some() {
                continue;
            }
            let expired = lifetime_ms > 0.0 && now_ms - line.line.at_ms as f64 >= lifetime_ms;
            if over > 0 || expired {
                over = over.saturating_sub(1);
                leave(line, now_ms);
                self.unkept = true;
            }
        }
    }

    fn leave_where(&mut self, matches: impl Fn(&ChatLine) -> bool) {
        let now_ms = js_sys::Date::now();
        for line in self.lines.iter_mut().filter(|line| line.remove_at_ms.is_none() && matches(&line.line)) {
            leave(line, now_ms);
            self.unkept = true;
        }
    }

    fn frame(&mut self, now_ms: f64) {
        self.trim(now_ms);
        self.lines.retain(|line| match line.remove_at_ms {
            Some(at) if now_ms >= at => {
                line.element.remove();
                false
            }
            _ => true,
        });
        if self.unkept && now_ms - self.kept_at_ms >= KEEP_EVERY_MS {
            self.unkept = false;
            self.kept_at_ms = now_ms;
            self.keep();
        }
    }

    /// Keeps the lines staying on the page with the app (not samples).
    fn keep(&self) {
        let lines: Vec<ChatLine> = self
            .lines
            .iter()
            .filter(|line| line.remove_at_ms.is_none() && !line.sample)
            .map(|line| line.line.clone())
            .collect();
        let skip = lines.len().saturating_sub(MAX_KEPT);
        let kept = Kept { lines: lines.into_iter().skip(skip).collect() };
        let message = PageMessage::Keep { value: serde_json::to_value(kept).expect("lines serialise") };
        let message = serde_json::to_string(&message).expect("page messages serialise");
        let parent = web_sys::window().and_then(|window| window.parent().ok().flatten());
        if let (Some(parent), Ok(message)) = (parent, js_sys::JSON::parse(&message)) {
            let _ = parent.post_message(&message, "*");
        }
    }
}

/// Starts a line's leaving, timed by what its CSS makes of `msg--leaving`.
fn leave(line: &mut Line, now_ms: f64) {
    let _ = line.element.class_list().add_1("msg--leaving");
    line.remove_at_ms = Some(now_ms + leave_ms(&line.element));
}

/// The longest animation or transition on the element, delays included.
fn leave_ms(element: &Element) -> f64 {
    let Some(style) = web_sys::window().and_then(|window| window.get_computed_style(element).ok().flatten())
    else {
        return 0.0;
    };
    let value = |name: &str| style.get_property_value(name).unwrap_or_default();
    let longest = |durations: &str, delays: &str| {
        let durations: Vec<f64> = times(&value(durations));
        let delays: Vec<f64> = times(&value(delays));
        durations
            .iter()
            .enumerate()
            .map(|(index, duration)| {
                // Shorter lists repeat, as in CSS.
                let delay = if delays.is_empty() { 0.0 } else { delays[index % delays.len()] };
                duration + delay.max(0.0)
            })
            .fold(0.0, f64::max)
    };
    let animation = if value("animation-name").trim() == "none" {
        0.0
    } else {
        longest("animation-duration", "animation-delay")
    };
    let transition = longest("transition-duration", "transition-delay");
    animation.max(transition).min(MAX_LEAVE_MS)
}

/// `0.4s, 300ms` → milliseconds.
fn times(list: &str) -> Vec<f64> {
    list.split(',')
        .filter_map(|time| {
            let time = time.trim();
            if let Some(ms) = time.strip_suffix("ms") {
                ms.trim().parse().ok()
            } else {
                time.strip_suffix('s').and_then(|s| s.trim().parse::<f64>().ok()).map(|s| s * 1000.0)
            }
        })
        .collect()
}

fn start_animation_loop(page: Rc<RefCell<Page>>) {
    type FrameCallback = Closure<dyn FnMut(f64)>;
    let callback: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
    let next = Rc::clone(&callback);
    *callback.borrow_mut() = Some(Closure::new(move |_: f64| {
        // Wall-clock ms: lines' times are compared with it.
        page.borrow_mut().frame(js_sys::Date::now());
        request_frame(next.borrow().as_ref().expect("callback is set"));
    }));
    request_frame(callback.borrow().as_ref().expect("callback is set"));
}

fn request_frame(callback: &Closure<dyn FnMut(f64)>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
    }
}

fn document() -> Result<Document, JsValue> {
    web_sys::window().and_then(|w| w.document()).ok_or_else(|| "no document".into())
}

fn warn(message: &str) {
    web_sys::console::warn_1(&message.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::chat::ChatUser;

    #[test]
    fn css_times() {
        assert_eq!(times("0.4s, 300ms"), [400.0, 300.0]);
        assert_eq!(times("0s"), [0.0]);
        assert_eq!(times(""), Vec::<f64>::new());
    }

    #[test]
    fn settings_leave_out_commands_and_bots() {
        let settings = Settings::from_values(
            serde_json::json!({ "hidden_users": ["Nightbot", " StreamElements ", ""], "max_messages": 500 })
                .as_object()
                .unwrap(),
        );
        assert_eq!(settings.hidden_users, ["Nightbot", "StreamElements"]);
        assert_eq!(settings.max_messages, 200, "clamped");
        let line = |name: &str, text: &str, source: LineSource| ChatLine {
            id: "x".into(),
            platform: "twitch".into(),
            at_ms: 0,
            source,
            user: Some(ChatUser {
                name: name.into(),
                display_name: name.to_uppercase(),
                ..Default::default()
            }),
            parts: vec![protocol::AlertPart::Text { text: text.into(), style: None }],
            marks: ChatMarks::default(),
        };
        assert!(settings.leaves_out(&line("viewer", " !so someone", LineSource::Chat)));
        assert!(settings.leaves_out(&line("nightbot", "hi", LineSource::Chat)));
        assert!(!settings.leaves_out(&line("viewer", "hi !", LineSource::Chat)));
        assert!(
            !settings.leaves_out(&line("nightbot", "!x", LineSource::Card { class: String::new() })),
            "a card's"
        );
        let shown = Settings { show_commands: true, ..settings };
        assert!(!shown.leaves_out(&line("viewer", "!so", LineSource::Chat)));
        assert_eq!(
            names(Some(&serde_json::json!("Nightbot, StreamElements ,"))),
            ["Nightbot", "StreamElements"],
            "as it was saved before"
        );
    }
}
