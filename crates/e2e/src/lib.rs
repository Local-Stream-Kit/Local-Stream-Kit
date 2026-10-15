//! End-to-end tests for the control panel: a real `lsk` server in
//! a folder of its own, the real panel in a headless browser (Edge or
//! Chrome, whichever is installed), driven with real mouse and keyboard
//! events.
//!
//! Run them with `cargo e2e` (see `crates/xtask`): it builds the panel and
//! the server first, then runs `tests/` here. They're `#[ignore]`d so a
//! plain `cargo test` stays fast and needs no browser.
//!
//! Each test gets its own server (on a free port, with a fresh
//! `lsk.json` under `target/e2e/<test>/`, no Twitch channel and no
//! emote sources, so nothing goes over the network) and its own browser.
//! The folder is kept afterwards: `server.log` is there for a failed test.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, CaptureScreenshotParams,
};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures_util::{SinkExt, StreamExt};
use protocol::config::AppConfig;
use protocol::control::{ControlMessage, ControlRequest, ControlState};
use protocol::plugin::{ALERT_PLUGIN, PluginMessage};
use protocol::{ClientMessage, Command, Envelope, PluginCommand};
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use tokio_tungstenite::tungstenite::Message;

/// How long a `wait_*` waits before failing the test.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// How long a server may take to start ([`App::restart`]), and so a panel
/// to reconnect ([`Panel::wait_reconnected`]).
const START_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

/// The browser window, in CSS pixels. Wide, so the editor shows its side list.
pub const WIDTH: u32 = 1400;
pub const HEIGHT: u32 = 900;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("e2e lives in crates/e2e").to_path_buf()
}

fn target_dir() -> PathBuf {
    let root = workspace_root();
    std::env::var_os("CARGO_TARGET_DIR").map(|dir| root.join(dir)).unwrap_or_else(|| root.join("target"))
}

/// A config for tests: the premade cards, nothing that needs the network.
/// The OBS plugin looks for OBS where nothing listens (port 1), never at
/// the OBS of whoever runs the tests (`localhost:4455`).
pub fn test_config() -> AppConfig {
    let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
    config.connections.emote_sources.clear();
    obs_at(&mut config, "127.0.0.1:1");
    config
}

/// Points the OBS plugin at `address` (`host:port`).
pub fn obs_at(config: &mut AppConfig, address: &str) {
    let obs = config.plugins.entry("obs".into()).or_default();
    obs.enabled = true;
    obs.settings.insert("address".into(), address.into());
}

/// A running server with its own folder and port.
pub struct App {
    pub dir: PathBuf,
    pub port: u16,
    /// Passed to the server, e.g. `--debug`.
    args: Vec<String>,
    /// The server's `RUST_LOG` (`lsk=debug`); `None` leaves it unset, so
    /// the Detailed log setting decides. Applies from the next restart.
    pub rust_log: Option<String>,
    server: Option<Child>,
    /// Where the server's last start begins in `server.log`, which keeps
    /// every start's lines.
    log_from: u64,
}

impl App {
    /// Starts the server built by `cargo e2e` in `target/e2e/<name>/`,
    /// with `config` (its port is replaced by a free one).
    pub async fn start(name: &str, config: AppConfig) -> App {
        App::start_with(name, config, &[]).await
    }

    /// As [`App::start`], with `--debug`: [`Panel::input`] can send it
    /// events as if they came from Twitch.
    pub async fn start_debug(name: &str, config: AppConfig) -> App {
        App::start_with(name, config, &["--debug"]).await
    }

    /// As [`App::start`], with these arguments (`--debug`, the hidden
    /// `--update-…` ones).
    pub async fn start_with(name: &str, mut config: AppConfig, args: &[&str]) -> App {
        let dir = target_dir().join("e2e").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test folder");
        let port = free_port();
        config.connections.overlay_port = port;
        let json = serde_json::to_string_pretty(&config).expect("configs serialise");
        std::fs::write(dir.join("lsk.json"), json).expect("write the test config");

        let args = args.iter().map(|arg| arg.to_string()).collect();
        let mut app = App { dir, port, args, rust_log: Some("lsk=debug".into()), server: None, log_from: 0 };
        app.restart().await;
        app
    }

    /// Stops the server, as if it was closed.
    pub async fn stop(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.kill().await;
        }
    }

    /// Starts the server again (stopping it first if it runs), with the
    /// same folder, config and port.
    pub async fn restart(&mut self) {
        self.stop().await;
        let exe = target_dir().join("debug").join(format!("lsk{}", std::env::consts::EXE_SUFFIX));
        assert!(exe.is_file(), "{} is missing; run the tests with `cargo e2e`", exe.display());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
            .expect("open server.log");
        self.log_from = log.metadata().map_or(0, |metadata| metadata.len());
        let mut command = tokio::process::Command::new(&exe);
        match &self.rust_log {
            Some(filter) => command.env("RUST_LOG", filter),
            None => command.env_remove("RUST_LOG"),
        };
        let server = command
            .args(&self.args)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone the log file"))
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|error| panic!("couldn't start {}: {error}", exe.display()));
        self.server = Some(server);

        let deadline = Instant::now() + START_TIMEOUT;
        while TcpStream::connect(("127.0.0.1", self.port)).await.is_err() {
            assert!(Instant::now() < deadline, "the server didn't start; see {}", self.log_path().display());
            sleep(POLL).await;
        }
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.join("server.log")
    }

    /// Waits until the server code of each of `plugins` is running (it
    /// compiled, and `start` was called) since the server last started.
    /// What's sent to it meanwhile waits for it, but a debug build takes a
    /// second or more to compile one, and longer under the full suite's
    /// load, so a test that times what comes back (an overlay's
    /// [`TIMEOUT`]) waits for this first.
    pub async fn wait_running(&self, plugins: &[&str]) {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            let log = std::fs::read(self.log_path()).unwrap_or_default();
            let log = String::from_utf8_lossy(log.get(self.log_from as usize..).unwrap_or_default());
            let running = |plugin: &&str| {
                let quoted = format!("\"{plugin}\"");
                log.lines().any(|line| {
                    line.contains("the plugin's server code is running") && line.contains(&quoted)
                })
            };
            if plugins.iter().all(running) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the server code of {plugins:?} to run; see {}",
                self.log_path().display()
            );
            sleep(POLL).await;
        }
    }

    /// The panel's address for a route such as `cards` or `card/hydrate`.
    pub fn panel_url(&self, route: &str) -> String {
        format!("http://127.0.0.1:{}/panel/#/{route}", self.port)
    }

    /// The config as the server last wrote it.
    pub fn saved_config(&self) -> AppConfig {
        let text = std::fs::read_to_string(self.dir.join("lsk.json")).expect("read the config");
        serde_json::from_str(&text).expect("the saved config parses")
    }

    /// Waits until the saved config passes `check`, and returns it.
    pub async fn wait_for_config(&self, what: &str, check: impl Fn(&AppConfig) -> bool) -> AppConfig {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            // The server writes a temporary file and renames it, so a read
            // never sees half a file; a failed parse just means "not yet".
            let text = std::fs::read_to_string(self.dir.join("lsk.json")).unwrap_or_default();
            if let Ok(config) = serde_json::from_str::<AppConfig>(&text)
                && check(&config)
            {
                return config;
            }
            assert!(Instant::now() < deadline, "timed out waiting for the saved config: {what}");
            sleep(POLL).await;
        }
    }

    /// Connects a stand-in overlay that hosts these plugins' pages (none
    /// named: as without `?modules=`, every one but those with a source of
    /// their own). It plays the queue in no time: it's done with each
    /// queued run as soon as it sees it.
    pub async fn overlay(&self, pages: Vec<&str>) -> Overlay {
        self.connect_overlay(pages, true).await
    }

    /// As [`App::overlay`], but it's done with an item only when told
    /// ([`Overlay::done`]).
    pub async fn overlay_holding(&self, pages: Vec<&str>) -> Overlay {
        self.connect_overlay(pages, false).await
    }

    /// Connects a stand-in overlay whose link names the overlay `id`
    /// (`?overlay=id`), playing the queue as [`App::overlay`].
    pub async fn named_overlay(&self, id: &str) -> Overlay {
        self.connect(ClientMessage::Hello { overlay: Some(id.to_string()), plugins: None }, true).await
    }

    /// The control API's key, as the server saved it.
    pub fn control_key(&self) -> String {
        let text = std::fs::read_to_string(self.dir.join("lsk_control.json")).expect("read the keys");
        let keys: serde_json::Value = serde_json::from_str(&text).expect("the keys parse");
        keys["keys"][0]["key"].as_str().expect("a key").to_string()
    }

    /// Connects to the control API's WebSocket, like the Stream Deck plugin.
    pub async fn control(&self) -> Control {
        let url = format!(
            "ws://127.0.0.1:{}{}?{}={}",
            self.port,
            protocol::control::CONTROL_WS_PATH,
            protocol::control::KEY_PARAM,
            self.control_key()
        );
        let (socket, _) = tokio_tungstenite::connect_async(url).await.expect("connect to the control API");
        Control { socket }
    }

    async fn connect_overlay(&self, pages: Vec<&str>, plays: bool) -> Overlay {
        let plugins = (!pages.is_empty()).then(|| pages.iter().map(|page| page.to_string()).collect());
        self.connect(ClientMessage::Hello { overlay: None, plugins }, plays).await
    }

    async fn connect(&self, hello: ClientMessage, plays: bool) -> Overlay {
        let url = format!("ws://127.0.0.1:{}{}", self.port, protocol::WS_PATH);
        let (mut socket, _) = tokio_tungstenite::connect_async(url).await.expect("connect the overlay");
        let hello = serde_json::to_string(&hello).expect("hello serialises");
        socket.send(Message::text(hello)).await.expect("send hello");
        Overlay { socket, plays }
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("a local address").port()
}

/// A control API client on the WebSocket.
pub struct Control {
    socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
}

impl Control {
    pub async fn send(&mut self, request: &ControlRequest) {
        let json = serde_json::to_string(request).expect("requests serialise");
        self.socket.send(Message::text(json)).await.expect("send a control request");
    }

    /// Waits for a message that `pick` takes, skipping the others.
    pub async fn wait_for<T>(&mut self, what: &str, pick: impl Fn(&ControlMessage) -> Option<T>) -> T {
        self.wait_at_most(TIMEOUT, pick).await.unwrap_or_else(|| panic!("timed out waiting for {what}"))
    }

    /// As [`Control::wait_for`], giving up after `time`.
    pub async fn wait_at_most<T>(
        &mut self,
        time: Duration,
        pick: impl Fn(&ControlMessage) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + time;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = tokio::time::timeout(left, self.socket.next())
                .await
                .ok()?
                .expect("the control API connection is open")
                .expect("a message");
            if let Message::Text(text) = message {
                let message: ControlMessage = serde_json::from_str(&text).expect("a control message");
                if let Some(found) = pick(&message) {
                    return Some(found);
                }
            }
        }
    }

    /// Waits for a pushed state that passes `check`.
    pub async fn wait_for_state(
        &mut self,
        what: &str,
        check: impl Fn(&ControlState) -> bool,
    ) -> ControlState {
        self.wait_for(what, |message| match message {
            ControlMessage::State { state } if check(state) => Some(state.clone()),
            _ => None,
        })
        .await
    }
}

/// A fake overlay page: sees the commands the server sends.
pub struct Overlay {
    socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    /// Done with each queue item as soon as it's seen.
    plays: bool,
}

impl Overlay {
    /// The next command matching `pick`, skipping others.
    pub async fn wait_for<T>(&mut self, what: &str, pick: impl Fn(&Command) -> Option<T>) -> T {
        self.wait_at_most(TIMEOUT, pick)
            .await
            .unwrap_or_else(|| panic!("timed out waiting for the overlay to get {what}"))
    }

    /// As [`Overlay::wait_for`], giving up after `time`.
    pub async fn wait_at_most<T>(
        &mut self,
        time: Duration,
        pick: impl Fn(&Command) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + time;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = tokio::time::timeout(left, self.socket.next()).await.ok()?;
            let Some(Ok(Message::Text(text))) = message else { continue };
            let envelope: Envelope = serde_json::from_str(&text).expect("envelopes parse");
            if self.plays
                && let Command::Plugin(PluginCommand {
                    message: PluginMessage::Run { queue_id: Some(id), .. },
                    ..
                }) = &envelope.command
            {
                self.done(*id).await;
            }
            if let Some(found) = pick(&envelope.command) {
                return Some(found);
            }
        }
    }

    /// Tells the server queue item `id` has finished playing here.
    pub async fn done(&mut self, id: u64) {
        let done = serde_json::to_string(&ClientMessage::Done { id }).expect("done serialises");
        self.socket.send(Message::text(done)).await.expect("send done");
    }

    /// The next alert (an Alerts plugin run): its queue item's id and its
    /// message's text.
    pub async fn next_show(&mut self) -> (u64, String) {
        self.wait_for("an alert", alert_shown).await
    }

    /// The text of the next alert.
    pub async fn next_alert(&mut self) -> String {
        self.next_show().await.1
    }
}

/// An Alerts plugin run: its queue id and its message's text.
pub fn alert_shown(command: &Command) -> Option<(u64, String)> {
    match command {
        Command::Plugin(PluginCommand { plugin, message: PluginMessage::Run { queue_id, values, .. } })
            if plugin == ALERT_PLUGIN =>
        {
            Some((
                queue_id.unwrap_or_default(),
                values["message"]["text"].as_str().unwrap_or_default().to_string(),
            ))
        }
        _ => None,
    }
}

/// A queued run skipped: its queue id.
pub fn skipped(command: &Command) -> Option<u64> {
    match command {
        Command::Plugin(PluginCommand { message: PluginMessage::Skip { queue_id }, .. }) => Some(*queue_id),
        _ => None,
    }
}

/// Modifier keys, as the DevTools protocol numbers them.
pub mod modifiers {
    pub const NONE: i64 = 0;
    pub const ALT: i64 = 1;
    pub const CTRL: i64 = 2;
    pub const SHIFT: i64 = 8;
}

/// A key for [`Panel::key`].
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub key: &'static str,
    pub code: &'static str,
    pub key_code: i64,
}

impl Key {
    pub const ENTER: Key = Key { key: "Enter", code: "Enter", key_code: 13 };
    pub const ESCAPE: Key = Key { key: "Escape", code: "Escape", key_code: 27 };
    pub const DELETE: Key = Key { key: "Delete", code: "Delete", key_code: 46 };
    pub const BACKSPACE: Key = Key { key: "Backspace", code: "Backspace", key_code: 8 };
    pub const HOME: Key = Key { key: "Home", code: "Home", key_code: 36 };
    pub const END: Key = Key { key: "End", code: "End", key_code: 35 };
    pub const ARROW_DOWN: Key = Key { key: "ArrowDown", code: "ArrowDown", key_code: 40 };
    pub const ARROW_UP: Key = Key { key: "ArrowUp", code: "ArrowUp", key_code: 38 };
    pub const ARROW_LEFT: Key = Key { key: "ArrowLeft", code: "ArrowLeft", key_code: 37 };

    /// A letter key; `key` is what a shortcut handler sees (`"a"`, or `"A"`
    /// with Shift).
    pub const fn letter(key: &'static str, code: &'static str, key_code: i64) -> Key {
        Key { key, code, key_code }
    }

    pub const A: Key = Key::letter("a", "KeyA", 65);
    pub const SHIFT_A: Key = Key::letter("A", "KeyA", 65);
    pub const D: Key = Key::letter("d", "KeyD", 68);
    pub const X: Key = Key::letter("x", "KeyX", 88);
    pub const Y: Key = Key::letter("y", "KeyY", 89);
    pub const Z: Key = Key::letter("z", "KeyZ", 90);
}

/// The control panel (or another of the app's pages, such as the overlay)
/// open in a headless browser.
pub struct Panel {
    pub page: Page,
    browser: Browser,
    handler: JoinHandle<()>,
}

impl Panel {
    /// Opens the panel at `route` and waits until it has the config.
    pub async fn open(app: &App, route: &str) -> Panel {
        let panel = Panel::launch(app, &app.panel_url(route)).await;
        panel.wait("the panel to load its config", "document.querySelector('.dock, .editor')").await;
        panel
    }

    /// Opens the overlay page as OBS would, with `query` (`?modules=…`, or
    /// empty). Needs the overlay built (`cargo e2e` builds it).
    pub async fn open_overlay(app: &App, query: &str) -> Panel {
        Panel::launch(app, &format!("http://127.0.0.1:{}/{query}", app.port)).await
    }

    /// A screenshot of the page as base64 PNG, also saved to `path`.
    pub async fn screenshot(&self, path: &Path) -> String {
        let shot = self.page.execute(CaptureScreenshotParams::default()).await.expect("take a screenshot");
        let base64: &str = shot.result.data.as_ref();
        let _ = self.page.save_screenshot(CaptureScreenshotParams::default(), path).await;
        base64.to_string()
    }

    async fn launch(app: &App, url: &str) -> Panel {
        let config = BrowserConfig::builder()
            .user_data_dir(app.dir.join("browser"))
            .window_size(WIDTH, HEIGHT)
            .viewport(Viewport {
                width: WIDTH,
                height: HEIGHT,
                device_scale_factor: Some(1.0),
                ..Viewport::default()
            })
            .build()
            .expect("a browser config");
        let (browser, mut handler) = Browser::launch(config)
            .await
            .expect("couldn't start a browser; install Edge or Chrome (or set CHROME to its path)");
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let page = browser.new_page("about:blank").await.expect("open a page");
        page.evaluate_on_new_document(AddScriptToEvaluateOnNewDocumentParams::new(SOCKET_HOOK))
            .await
            .expect("add the socket hook");
        page.goto(url).await.expect("open the page");
        Panel { page, browser, handler }
    }

    /// Closes the browser. (Dropping the panel kills it too.)
    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler.abort();
    }

    /// Puts a file into a file input, as if picked in the file dialog.
    pub async fn choose_file(&self, selector: &str, path: &Path) {
        let input = self.page.find_element(selector).await.expect("the file input");
        let params = SetFileInputFilesParams::builder()
            .files(vec![path.display().to_string()])
            .backend_node_id(input.backend_node_id)
            .build()
            .expect("file input params");
        self.page.execute(params).await.expect("choose the file");
    }

    /// Resizes the page, e.g. to the width of an OBS dock.
    pub async fn resize(&self, width: u32, height: u32) {
        let size = SetDeviceMetricsOverrideParams::new(width as i64, height as i64, 1.0, false);
        self.page.execute(size).await.expect("resize the page");
    }

    /// Goes to another route without reloading.
    pub async fn go(&self, route: &str) {
        self.run(&format!("location.hash = '#/{route}'")).await;
    }

    /// Evaluates a JavaScript expression (it may be a promise).
    pub async fn eval<T: DeserializeOwned>(&self, expression: &str) -> T {
        self.call(&format!("return ({expression});")).await
    }

    /// Runs JavaScript statements.
    pub async fn run(&self, statements: &str) {
        self.call::<serde_json::Value>(&format!("{statements}; return null;")).await;
    }

    async fn call<T: DeserializeOwned>(&self, body: &str) -> T {
        let value =
            self.try_call(body).await.unwrap_or_else(|error| panic!("JavaScript failed: {error}\n{body}"));
        serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("unexpected JavaScript result {value}: {error}\n{body}"))
    }

    async fn try_call(&self, body: &str) -> chromiumoxide::Result<serde_json::Value> {
        let function = format!("async () => {{ {body} }}");
        let result = self.page.evaluate_function(function.as_str()).await?;
        Ok(result.value().cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Waits until the expression is truthy. Errors count as "not yet": the
    /// page may be reloading.
    pub async fn wait(&self, what: &str, expression: &str) {
        self.wait_within(TIMEOUT, what, expression).await;
    }

    /// As [`Panel::wait`], for at most `time`.
    async fn wait_within(&self, time: Duration, what: &str, expression: &str) {
        let deadline = Instant::now() + time;
        let check = format!("return !!({expression});");
        while self.try_call(&check).await.ok() != Some(serde_json::Value::Bool(true)) {
            assert!(Instant::now() < deadline, "timed out waiting for {what} ({expression})");
            sleep(POLL).await;
        }
    }

    /// How many WebSockets the page has opened: one more each time the
    /// panel reconnects. Read it before the server goes away, for
    /// [`Panel::wait_reconnected`].
    pub async fn sockets_opened(&self) -> usize {
        self.eval("window.__panelSockets.length").await
    }

    /// Waits until the panel has connected again since it had opened
    /// `opened` sockets ([`Panel::sockets_opened`]), and the server has
    /// sent `what` on that connection (a `PanelEvent`'s type, such as
    /// `queue`). Until then the panel still shows what it had before, and
    /// what it's asked to send (a click on Resume) goes nowhere. It retries
    /// 0.5 s after losing the server, then 1 s, 2 s, 4 s later…, so a
    /// server slow to start (a busy machine) can make this take more than
    /// [`TIMEOUT`].
    pub async fn wait_reconnected(&self, opened: usize, what: &str) {
        let check = format!(
            "window.__panelSockets.length > {opened} && window.__panelSockets.at(-1).readyState === 1 \
             && window.__panelSockets.at(-1).__received.includes({})",
            js_string(what)
        );
        self.wait_within(START_TIMEOUT, &format!("the panel to reconnect and get {what}"), &check).await;
    }

    /// The number of elements matching a CSS selector.
    pub async fn count(&self, selector: &str) -> usize {
        self.eval(&format!("document.querySelectorAll({}).length", js_string(selector))).await
    }

    /// The centre of the first element matching a CSS selector, in page
    /// pixels. An element outside the window is scrolled into it first, as
    /// a person would.
    pub async fn center(&self, selector: &str) -> (f64, f64) {
        let found: Option<(f64, f64)> = self
            .eval(&format!(
                "(() => {{ const e = document.querySelector({}); if (!e) return null; \
                 let r = e.getBoundingClientRect(); \
                 if (r.bottom > innerHeight || r.top < 0) {{ e.scrollIntoView({{ block: 'center' }}); \
                 r = e.getBoundingClientRect(); }} \
                 return [r.x + r.width / 2, r.y + r.height / 2]; }})()",
                js_string(selector)
            ))
            .await;
        found.unwrap_or_else(|| panic!("nothing matches {selector}"))
    }

    async fn mouse(&self, kind: DispatchMouseEventType, at: (f64, f64), pressed: bool, modifiers: i64) {
        self.mouse_button(kind, at, MouseButton::Left, pressed, modifiers).await;
    }

    async fn mouse_button(
        &self,
        kind: DispatchMouseEventType,
        (x, y): (f64, f64),
        button: MouseButton,
        pressed: bool,
        modifiers: i64,
    ) {
        let held = match button {
            MouseButton::Right => 2,
            _ => 1,
        };
        let event = DispatchMouseEventParams::builder()
            .r#type(kind)
            .x(x)
            .y(y)
            .button(button)
            .buttons(if pressed { held } else { 0 })
            .click_count(1)
            .modifiers(modifiers)
            .build()
            .expect("a mouse event");
        self.page.execute(event).await.expect("dispatch a mouse event");
    }

    /// A left click at a point.
    pub async fn click_at(&self, at: (f64, f64), modifiers: i64) {
        self.mouse(DispatchMouseEventType::MouseMoved, at, false, modifiers).await;
        self.mouse(DispatchMouseEventType::MousePressed, at, true, modifiers).await;
        self.mouse(DispatchMouseEventType::MouseReleased, at, false, modifiers).await;
    }

    /// A right click at a point (the browser's context menu event follows).
    pub async fn right_click_at(&self, at: (f64, f64)) {
        self.mouse(DispatchMouseEventType::MouseMoved, at, false, modifiers::NONE).await;
        self.mouse_button(
            DispatchMouseEventType::MousePressed,
            at,
            MouseButton::Right,
            true,
            modifiers::NONE,
        )
        .await;
        self.mouse_button(
            DispatchMouseEventType::MouseReleased,
            at,
            MouseButton::Right,
            false,
            modifiers::NONE,
        )
        .await;
    }

    /// The mouse wheel turned at a point (`delta_y` > 0: down).
    pub async fn wheel(&self, (x, y): (f64, f64), delta_y: f64) {
        self.mouse(DispatchMouseEventType::MouseMoved, (x, y), false, modifiers::NONE).await;
        let event = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseWheel)
            .x(x)
            .y(y)
            .delta_x(0.0)
            .delta_y(delta_y)
            .build()
            .expect("a wheel event");
        self.page.execute(event).await.expect("dispatch a wheel event");
    }

    /// A left click on the first element matching a CSS selector.
    pub async fn click(&self, selector: &str) {
        // Where it is once it holds still: a preview measuring itself above
        // it can still move it, and a click where it was misses.
        let mut at = self.center(selector).await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let now = self.center(selector).await;
            if now == at {
                break;
            }
            at = now;
        }
        self.click_at(at, modifiers::NONE).await;
    }

    /// Presses the left button at `from`, moves to `to` in steps, releases.
    pub async fn drag(&self, from: (f64, f64), to: (f64, f64), modifiers: i64) {
        self.mouse(DispatchMouseEventType::MouseMoved, from, false, modifiers).await;
        self.mouse(DispatchMouseEventType::MousePressed, from, true, modifiers).await;
        const STEPS: u32 = 8;
        for step in 1..=STEPS {
            let t = f64::from(step) / f64::from(STEPS);
            let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.mouse(DispatchMouseEventType::MouseMoved, at, true, modifiers).await;
        }
        self.mouse(DispatchMouseEventType::MouseReleased, to, false, modifiers).await;
    }

    /// Moves the mouse without pressing anything.
    pub async fn hover(&self, at: (f64, f64)) {
        self.mouse(DispatchMouseEventType::MouseMoved, at, false, modifiers::NONE).await;
    }

    /// Presses and releases a key, as a shortcut (no text is typed).
    pub async fn key(&self, key: Key, modifiers: i64) {
        for kind in [DispatchKeyEventType::RawKeyDown, DispatchKeyEventType::KeyUp] {
            let event = DispatchKeyEventParams::builder()
                .r#type(kind)
                .key(key.key)
                .code(key.code)
                .windows_virtual_key_code(key.key_code)
                .native_virtual_key_code(key.key_code)
                .modifiers(modifiers)
                .build()
                .expect("a key event");
            self.page.execute(event).await.expect("dispatch a key event");
        }
    }

    /// Types text into whatever has focus.
    pub async fn type_text(&self, text: &str) {
        self.page.execute(InsertTextParams::new(text)).await.expect("insert text");
    }

    /// Copies the selected nodes: fires a `copy` event as Ctrl+C would and
    /// returns what went on the clipboard. (The test doesn't touch the real
    /// clipboard, which is the person's.)
    pub async fn copy(&self) -> String {
        self.eval(
            "(() => { const data = new DataTransfer(); \
             document.body.dispatchEvent(new ClipboardEvent('copy', { clipboardData: data, bubbles: true, cancelable: true })); \
             return data.getData('text/plain'); })()",
        )
        .await
    }

    /// Pastes `text` as Ctrl+V would.
    pub async fn paste(&self, text: &str) {
        self.run(&format!(
            "const data = new DataTransfer(); data.setData('text/plain', {}); \
             document.body.dispatchEvent(new ClipboardEvent('paste', {{ clipboardData: data, bubbles: true, cancelable: true }}))",
            js_string(text)
        ))
        .await;
    }

    /// Sends the server an input as if it came from a platform (the app
    /// must run with `--debug`: [`App::start_debug`]).
    pub async fn input(&self, input: serde_json::Value) {
        let status: u16 = self
            .eval(&format!(
                "fetch('/debug/input', {{ method: 'POST', headers: {{ 'content-type': 'application/json' }}, body: {} }}).then(r => r.status)",
                js_string(&input.to_string())
            ))
            .await;
        assert_eq!(status, 202, "the input was taken");
    }

    /// A web request from the panel's page (`GET`, `POST`…): the status
    /// and the JSON answer.
    pub async fn request(&self, method: &str, path: &str) -> (u16, serde_json::Value) {
        self.eval(&format!(
            "fetch({}, {{ method: {} }}).then(async r => [r.status, await r.json()])",
            js_string(path),
            js_string(method)
        ))
        .await
    }

    /// Hands the panel a message as if the server had sent it on the panel
    /// WebSocket.
    pub async fn receive(&self, event: &protocol::panel::PanelEvent) {
        let json = serde_json::to_string(event).expect("panel events serialise");
        self.run(&format!(
            "const socket = window.__panelSockets.at(-1); \
             socket.onmessage(new MessageEvent('message', {{ data: {} }}))",
            js_string(&json)
        ))
        .await;
    }

    /// Waits until the panel says "Saved".
    pub async fn wait_saved(&self) {
        self.wait("the panel to save", "document.querySelector('.save-state')?.textContent === 'Saved'")
            .await;
    }
}

/// Runs before the panel's own code: keeps its WebSockets reachable, so
/// [`Panel::receive`] can hand it messages.
/// It also records the type of every message the server sends, in
/// `window.__panelReceived`, and each socket's own in its `__received`.
const SOCKET_HOOK: &str = "(() => { \
    const Native = window.WebSocket; window.__panelSockets = []; window.__panelReceived = []; \
    window.WebSocket = class extends Native { \
        constructor(...args) { \
            super(...args); window.__panelSockets.push(this); this.__received = []; \
            this.addEventListener('message', event => { \
                try { const type = JSON.parse(event.data).type; \
                      window.__panelReceived.push(type); this.__received.push(type); } catch {} }); } }; })()";

/// A Rust string as a JavaScript string literal.
pub fn js_string(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialise")
}

/// Selectors for the node editor.
pub mod select {
    /// A node by id.
    pub fn node(id: u32) -> String {
        format!(".node[data-node='{id}']")
    }

    /// A node's header (where it's dragged).
    pub fn head(id: u32) -> String {
        format!("{} .node-head", node(id))
    }

    pub fn output(node: u32, socket: &str) -> String {
        format!(".socket.out[data-node='{node}'][data-socket='{socket}']")
    }

    pub fn input(node: u32, socket: &str) -> String {
        format!(".socket.in[data-node='{node}'][data-socket='{socket}']")
    }
}

/// A wire from an output to an input, as `(node, socket)`.
pub fn wire(from: (u32, &str), to: (u32, &str)) -> protocol::cards::Wire {
    use protocol::cards::{SocketRef, Wire};
    Wire { from: SocketRef::new(from.0, from.1), to: SocketRef::new(to.0, to.1) }
}

/// A node of a plugin's, with these settings.
pub fn plugin_node(id: u32, plugin: &str, node: &str, settings: serde_json::Value) -> protocol::cards::Node {
    protocol::cards::Node {
        id,
        x: 300.0,
        y: 0.0,
        kind: protocol::cards::NodeKind::Plugin {
            plugin: plugin.into(),
            node: node.into(),
            settings: settings.as_object().unwrap().clone(),
            changes: Default::default(),
            voice: Default::default(),
            ty: Default::default(),
        },
    }
}

/// An Alerts plugin node showing `message` for 5 seconds.
pub fn alert_node(id: u32, message: &str) -> protocol::cards::Node {
    plugin_node(id, ALERT_PLUGIN, "alert", serde_json::json!({ "message": message, "duration": 5.0 }))
}

/// An event node, as node 1.
pub fn event_node(kind: protocol::cards::NodeKind) -> protocol::cards::Node {
    protocol::cards::Node { id: 1, x: 0.0, y: 0.0, kind }
}

/// Copies one of the repository's plugin folders (from the workspace
/// root: `examples/plugins/counter`) into the app's `plugins/`. Restart the
/// app to load it.
pub fn install_plugin(app: &App, folder: &str) {
    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("make the folder");
        for entry in std::fs::read_dir(from).expect("read the folder").map(Result::unwrap) {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).expect("copy a file");
            }
        }
    }
    let from = workspace_root().join(folder);
    let name = from.file_name().expect("a folder").to_owned();
    copy_dir(&from, &app.dir.join("plugins").join(name));
}

/// A small web server on a free local port standing for a tool on this PC
/// that plugins' server code may reach (not the app): `/to-app` redirects
/// to the app's `/control/state` on `app_port`; anything else answers 200
/// "hi", and a WebSocket opens and stays open. Its port.
pub async fn local_tool(app_port: u16) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = vec![0; 4096];
                let Ok(peeked) = stream.peek(&mut head).await else { return };
                let head = String::from_utf8_lossy(&head[..peeked]).to_ascii_lowercase();
                if head.contains("upgrade: websocket") {
                    if let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await {
                        while let Some(Ok(_)) = socket.next().await {}
                    }
                    return;
                }
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let path =
                    String::from_utf8_lossy(&request).split(' ').nth(1).unwrap_or_default().to_string();
                let answer = if path == "/to-app" {
                    format!(
                        "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:{app_port}/control/state\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi".to_string()
                };
                let _ = stream.write_all(answer.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}
