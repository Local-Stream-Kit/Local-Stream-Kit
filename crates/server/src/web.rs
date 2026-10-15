//! The local web server: the built overlay, the emote image proxy, the
//! overlay WebSocket, the control panel (see `panel.rs`), pictures and
//! sounds (`media.rs`), and (with `--debug`) input injection.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use protocol::config::{AppConfig, OverlayLink};
use protocol::plugin::{MediaKind, PickFile, PluginMessage};
use protocol::{ClientMessage, Command, ShellCommand};
use tokio::sync::{broadcast, mpsc};

use crate::control;
use crate::media::{self, MediaFolders};
use crate::model::Input;
use crate::panel::{self, PanelState};
use crate::plugins::files::PluginFiles;
use crate::plugins::memory::PageMemory;
use crate::plugins::{PluginFile, Plugins};
use crate::services::activity::Pages;
use crate::services::config_store::ConfigStore;
use crate::services::image_proxy::{ImageProxy, ProxyError};
use crate::services::overlay_hub::{OverlayHub, wanted_by};

/// The built overlay (`trunk build` output). Debug builds read it from disk on
/// each request; release builds embed it in the binary.
#[derive(rust_embed::Embed)]
#[folder = "../overlay/dist/"]
#[allow_missing = true]
struct OverlayAssets;

#[derive(Clone)]
pub struct AppState {
    pub hub: OverlayHub,
    pub images: Arc<ImageProxy>,
    /// Set with `--debug`: lets `POST /debug/input` inject inputs.
    pub debug_inputs: Option<mpsc::Sender<Input>>,
    pub panel: PanelState,
}

pub async fn serve(port: u16, state: AppState) -> anyhow::Result<()> {
    if OverlayAssets::get("index.html").is_none() {
        tracing::error!("the overlay hasn't been built: run `trunk build` in crates/overlay");
    }
    let mut router = Router::new()
        .route(protocol::WS_PATH, get(websocket))
        .route(protocol::EMOTE_ROUTE, get(emote_image))
        .route(protocol::panel::PANEL_WS_PATH, get(panel::websocket))
        .route("/panel", get(panel::redirect))
        .route(protocol::panel::PANEL_PATH, get(panel::index))
        .route("/panel/{*path}", get(panel::asset))
        .route(protocol::alert::PICTURES_ROUTE, get(media::serve_picture))
        .route(protocol::sound::SOUNDS_ROUTE, get(media::serve_sound))
        .route(protocol::speech::SPEECH_ROUTE, get(crate::services::speech::serve))
        .route(protocol::plugin::FILES_ROUTE, get(plugin_file))
        .route(protocol::plugin::ICON_ROUTE, get(plugin_icon))
        .route("/control", get(control::get_state))
        .route(protocol::control::CONTROL_PATH, get(control::get_state))
        .route("/control/state", get(control::get_state))
        .route(protocol::control::CONTROL_WS_PATH, get(control::websocket))
        .route("/control/cards/{id}/{set}", get(control::card).post(control::card))
        .route("/control/effects/{id}/{set}", get(control::effect).post(control::effect))
        .route("/control/queue/{action}", get(control::queue).post(control::queue))
        .route("/control/items/{id}/skip", get(control::skip_item).post(control::skip_item))
        .route("/control/mute/{set}", get(control::mute).post(control::mute))
        .route(protocol::control::HISTORY_SEEN_PATH, get(control::history_seen).post(control::history_seen))
        .route("/control/events/{name}", get(control::event).post(control::event))
        .route("/control/{*rest}", any(control::unknown))
        .route(
            protocol::panel::PICTURE_UPLOAD_ROUTE,
            post(media::upload_picture).layer(DefaultBodyLimit::max(media::MAX_UPLOAD_BYTES)),
        )
        .route(
            protocol::panel::SOUND_UPLOAD_ROUTE,
            post(media::upload_sound).layer(DefaultBodyLimit::max(media::MAX_UPLOAD_BYTES)),
        )
        .route(
            protocol::panel::PLUGIN_INSTALL_PATH,
            post(install_plugin).layer(DefaultBodyLimit::max(crate::plugins::MAX_INSTALL_BYTES)),
        );
    if state.debug_inputs.is_some() {
        router = router.route("/debug/input", post(debug_input));
        spawn_build_watcher(state.hub.clone());
    }
    let router = router.fallback(overlay_file).layer(axum::middleware::from_fn(local_host)).with_state(state);

    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(address).await.with_context(|| {
        format!("couldn't listen on {address}; is the port in use? Change overlay_port in lsk.json")
    })?;
    tracing::info!("overlay: http://localhost:{port}");
    tracing::info!("control panel: http://localhost:{port}{}", protocol::panel::PANEL_PATH);
    axum::serve(listener, router).await?;
    Ok(())
}

/// Answers 403 to a request addressed to a host name other than
/// `localhost` (DNS rebinding: a site whose name comes to point at
/// 127.0.0.1 would otherwise be same-origin with the app, past the panel's
/// upload header). An IP address is fine whatever it is: rebinding needs a
/// name. A request without a host (HTTP/1.0) isn't from a browser.
async fn local_host(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .map(|host| host.to_str().unwrap_or("?"))
        .or_else(|| request.uri().authority().map(|authority| authority.as_str()));
    if let Some(host) = host
        && !local_host_name(host)
    {
        tracing::warn!(host, "refused a request for another host name (DNS rebinding?)");
        return (StatusCode::FORBIDDEN, "only http://localhost or an IP address reaches the app")
            .into_response();
    }
    next.run(request).await
}

/// Whether `host` (a `Host` header: a name or address, maybe a port) is
/// `localhost` or an IP address.
fn local_host_name(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        // [::1]:5750
        Some(ipv6) => match ipv6.split_once(']') {
            Some((address, port)) if port.is_empty() || port.starts_with(':') => {
                return address.parse::<std::net::Ipv6Addr>().is_ok();
            }
            _ => return false,
        },
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    let name = name.strip_suffix('.').unwrap_or(name);
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::Ipv4Addr>().is_ok()
}

async fn overlay_file(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    match OverlayAssets::get(path) {
        Some(file) => {
            // index.html must never be cached (OBS caches aggressively), so a
            // rebuild shows up; the other files have content hashes in their
            // names and never change.
            let cache = if path == "index.html" { "no-cache" } else { "public, max-age=31536000, immutable" };
            (
                [(header::CONTENT_TYPE, file.metadata.mimetype().to_string()), (header::CACHE_CONTROL, cache.to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        None if path == "index.html" => (
            StatusCode::SERVICE_UNAVAILABLE,
            Html("<p>The overlay hasn't been built yet: run <code>trunk build</code> in <code>crates/overlay</code>.</p>"),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The origin a sandboxed plugin page has (an opaque one): what it fetches
/// from the app (its own files, emote images, pictures and sounds) is a
/// cross-origin request. Only pages with an opaque origin may read these,
/// not other websites.
pub(crate) const SANDBOXED: &str = "null";

/// The `Content-Security-Policy` of what's only ever an image (emotes, a
/// plugin's icon): opened on its own, nothing loads and nothing runs.
const IMAGE_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// An emote's image, from the image proxy: only ever an image (see
/// `image_proxy.rs`), and never a page, even opened on its own.
async fn emote_image(State(state): State<AppState>, Path((source, id)): Path<(String, String)>) -> Response {
    match state.images.get(&source, &id).await {
        Ok(image) => (
            [
                (header::CONTENT_TYPE, image.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
                (header::CONTENT_SECURITY_POLICY, IMAGE_POLICY.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                // Plugin pages decode them (the rain's, frame by frame).
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, SANDBOXED.to_string()),
            ],
            image.bytes,
        )
            .into_response(),
        Err(error @ (ProxyError::UnknownEmote | ProxyError::NotFound)) => {
            tracing::debug!(source, id, %error, "emote image not available");
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error) => {
            tracing::warn!(source, id, %error, "couldn't fetch emote image");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn debug_input(State(state): State<AppState>, Json(input): Json<Input>) -> StatusCode {
    match &state.debug_inputs {
        Some(inputs) if inputs.send(input).await.is_ok() => StatusCode::ACCEPTED,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// A file of a plugin's overlay page. Plugin pages may only load from and
/// connect to the app itself (see docs/writing-a-plugin.md → Overlay pages).
async fn plugin_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin, path)): Path<(String, String)>,
) -> Response {
    let bytes = match state.panel.plugins.overlay_file(&plugin, &path) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(PluginFile::Bundled(bytes)) => bytes.into_owned(),
        Some(PluginFile::Disk(file)) => match tokio::fs::read(&file).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(plugin, path, %error, "couldn't read a plugin's file");
                return StatusCode::NOT_FOUND.into_response();
            }
        },
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    let page_hosts = state
        .panel
        .plugins
        .catalog()
        .plugin(&plugin)
        .map(|info| {
            let config = state.panel.store.current();
            protocol::plugin::allowed_page_hosts(info, &config.plugin(&plugin).page_hosts)
        })
        .unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, mime),
            // Plugin authors edit these while trying them out.
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (header::CONTENT_SECURITY_POLICY, plugin_policy(&headers, &page_hosts)),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            // A page's module scripts and WebAssembly load with CORS.
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, SANDBOXED.to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// A plugin's nodes' icon, which the panel draws as a mask: an image, never
/// a page (no scripts, nothing loaded, if opened on its own).
async fn plugin_icon(State(state): State<AppState>, Path(plugin): Path<String>) -> Response {
    let Some(bytes) = state.panel.plugins.icon(&plugin) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, IMAGE_POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes.into_owned(),
    )
        .into_response()
}

/// The app's own address and nothing else (plus `data:` and `blob:`, and
/// the page hosts allowed for it, over https). Named by the request's host
/// rather than `'self'`: a sandboxed iframe's origin is opaque. Over http
/// only: none of the app's WebSockets is for a plugin's page (it hears
/// what it's meant to through the overlay page).
fn plugin_policy(headers: &HeaderMap, page_hosts: &[String]) -> String {
    let own = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .filter(|host| {
            !host.is_empty()
                && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']'))
        })
        .map_or_else(|| "'self'".to_string(), |host| format!("http://{host}"));
    let others: String = page_hosts.iter().map(|host| format!(" https://{host}")).collect();
    format!(
        "default-src {own}{others} data: blob:; script-src {own}{others} 'unsafe-inline' 'wasm-unsafe-eval' blob:; \
         style-src {own}{others} 'unsafe-inline'; object-src 'none'; base-uri 'none'"
    )
}

/// `POST /panel/plugins`: a plugin as a zip, from the panel's Plugins page
/// (with the upload header, which other sites can't send). Answers the
/// plugin's id, or why it can't be installed.
async fn install_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !headers.contains_key(protocol::panel::UPLOAD_HEADER) {
        return (StatusCode::FORBIDDEN, "uploads only come from the control panel").into_response();
    }
    let plugins = Arc::clone(&state.panel.plugins);
    let installed = tokio::task::spawn_blocking(move || plugins.install_zip(&body)).await;
    match installed {
        Ok(Ok(id)) => {
            state.panel.store.set_plugins(state.panel.plugins.catalog()).await;
            id.into_response()
        }
        Ok(Err(error)) => {
            tracing::warn!("a plugin wasn't installed: {error}");
            (StatusCode::BAD_REQUEST, error).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// The overlay's WebSocket. Only for the overlay page itself (OBS's Browser
/// Source, a browser tab on this PC) and clients that aren't web pages: a
/// page on another site, or a plugin's page in its sandboxed iframe
/// (`Origin: null`), would hear every chat line and could end queue items
/// and keep fake chat lines.
async fn websocket(State(state): State<AppState>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if !panel::local_origin(&headers) {
        let origin = headers.get(header::ORIGIN).and_then(|origin| origin.to_str().ok()).unwrap_or("?");
        tracing::warn!(origin, "refused an overlay connection from a page on another site");
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(move |socket| overlay_connection(socket, state.hub, state.panel))
}

/// The plugin pages an overlay hosts, of plugins that are on: the ones its
/// overlay shows (`config.overlays`) or its `?modules=` names, or every
/// one but those with a Browser Source of their own (the chat display).
fn hosted_pages(plugins: &Plugins, config: &AppConfig, link: &OverlayLink) -> Vec<String> {
    let wanted = config.overlay_pages(link).plugins;
    let catalog = plugins.catalog();
    plugins
        .pages()
        .into_iter()
        .filter(|page| config.plugin(page).enabled)
        .filter(|page| match &wanted {
            Some(wanted) => wanted.contains(page),
            None => !catalog.plugin(page).is_some_and(|info| info.manifest.own_source),
        })
        .collect()
}

/// A page's plugin-wide settings, every one with a value (a file
/// setting's text; a folder's file `pick`ed); `None` for a plugin without
/// any.
fn page_settings(
    plugins: &Plugins,
    config: &AppConfig,
    files: &PluginFiles,
    id: &str,
    pick: &mut PickFile<'_>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let catalog = plugins.catalog();
    let manifest = &catalog.plugin(id)?.manifest;
    (!manifest.settings.is_empty()).then(|| {
        let stored = config.plugin(id);
        let mut values =
            protocol::plugin::shared_settings(&manifest.settings, stored.shared_settings(), pick);
        values.extend(files.values(id, stored.shared_settings()));
        values
    })
}

/// For telling whether a page's settings changed: a folder stays itself,
/// so another pick from it isn't a change.
fn folder_as_is(_: MediaKind, folder: &str) -> Option<String> {
    Some(format!("{folder}/"))
}

/// One overlay: waits for its `Hello`, sends the overlay build, the
/// plugin pages it hosts (with their settings and what they kept) and the
/// master volume, then relays every command for those pages and the
/// shell. Its plugin pages play the queue: it gets what's on stage, and
/// says when it's done.
async fn overlay_connection(socket: WebSocket, hub: OverlayHub, panel: PanelState) {
    let PanelState { status, activity, plugins: installed, store, memory, files, media, .. } = panel;
    let (mut sender, mut receiver) = socket.split();
    let mut commands = hub.subscribe();
    let mut config_changes = store.subscribe();
    let mut plugin_changes = installed.subscribe();
    let mut file_changes = files.subscribe();
    // What its link asked for; `None` until its hello.
    let mut link: Option<OverlayLink> = None;
    let mut plugins: Vec<String> = Vec::new();
    // The settings each page was sent last.
    let mut sent_settings: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    // Counts this overlay on the Status page while it's connected.
    let mut connected = None;
    // Counts its plugin pages, for their queued effects.
    let mut pages: Option<Pages> = None;
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let text = match incoming {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => {
                        tracing::debug!(%error, "overlay connection error");
                        break;
                    }
                };
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Hello { overlay, plugins: modules }) => {
                        // Its overlay's pages, or the ones named in `?modules=`.
                        let config = store.current();
                        let asked = OverlayLink { overlay, modules };
                        let overlay = config.overlay_pages(&asked).overlay;
                        plugins = hosted_pages(&installed, &config, &asked);
                        link = Some(asked);
                        sent_settings.clear();
                        tracing::info!(?overlay, ?plugins, "overlay connected");
                        connected = Some(status.overlay_connected(overlay, plugins.clone()));
                        let build = overlay_build()
                            .map(|id| hub.envelope(Command::Shell(ShellCommand::Build { id })));
                        pages = Some(activity.pages(plugins.clone()));
                        let on_stage = activity
                            .on_stage()
                            .map(|command| hub.envelope(command))
                            .filter(|envelope| wanted_by(envelope, &plugins) && !for_server(envelope, &installed));
                        let pages = hub.envelope(Command::Shell(ShellCommand::Plugins { plugins: plugins.clone() }));
                        let settings: Vec<_> = plugins
                            .iter()
                            .filter_map(|id| {
                                let seen = page_settings(&installed, &config, &files, id, &mut folder_as_is)?;
                                sent_settings.insert(id.clone(), seen);
                                let values = page_settings(&installed, &config, &files, id, &mut |kind, folder| media.pick_any(kind, folder))?;
                                Some(settings_envelope(&hub, id, values))
                            })
                            .collect();
                        let kept: Vec<_> = plugins.iter().filter_map(|id| kept_envelope(&hub, &memory, id)).collect();
                        let envelopes = build
                            .into_iter()
                            .chain([pages])
                            .chain(settings)
                            .chain(kept)
                            .chain(hub.configure_envelopes())
                            .chain(on_stage);
                        for envelope in envelopes {
                            if send(&mut sender, &envelope).await.is_err() {
                                return;
                            }
                        }
                    }
                    Ok(ClientMessage::Done { id }) => activity.done(id),
                    // Only a page this overlay hosts keeps anything.
                    Ok(ClientMessage::Keep { plugin, value }) => {
                        if plugins.contains(&plugin) {
                            memory.keep(&plugin, value);
                        }
                    }
                    Err(error) => tracing::warn!(%error, "unexpected message from the overlay"),
                }
            }
            command = commands.recv() => match command {
                Ok(envelope) => {
                    if wanted_by(&envelope, &plugins)
                        && !for_server(&envelope, &installed)
                        && send(&mut sender, &envelope).await.is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "overlay fell behind; skipping commands");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // A plugin switched on or off, installed, removed, or its
            // settings changed: the pages follow.
            changed = config_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                config_changes.borrow_and_update();
                if update_pages(&mut sender, &hub, &installed, &store, &files, &memory, &media, &link, &mut plugins, &mut sent_settings).await.is_err() {
                    break;
                }
            }
            // A file setting changed: the settings of its pages.
            changed = file_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                file_changes.borrow_and_update();
                if update_pages(&mut sender, &hub, &installed, &store, &files, &memory, &media, &link, &mut plugins, &mut sent_settings).await.is_err() {
                    break;
                }
            }
            changed = plugin_changes.changed() => {
                if changed.is_err() {
                    break;
                }
                plugin_changes.borrow_and_update();
                if update_pages(&mut sender, &hub, &installed, &store, &files, &memory, &media, &link, &mut plugins, &mut sent_settings).await.is_err() {
                    break;
                }
            }
        }
        // Its pages changed: the queue knows (the new count before the old
        // one goes, so what's on stage stays).
        if pages.as_ref().is_some_and(|pages| pages.plugins() != plugins) {
            pages = Some(activity.pages(plugins.clone()));
            if let Some(connected) = &connected {
                connected.set_plugins(plugins.clone());
            }
        }
    }
    tracing::info!("overlay disconnected");
}

/// Runs of a plugin with server code go there, not to its page (queued ones
/// too).
fn for_server(envelope: &protocol::Envelope, installed: &Plugins) -> bool {
    matches!(&envelope.command, Command::Plugin(command)
        if matches!(command.message, PluginMessage::Run { .. }) && installed.has_server(&command.plugin))
}

fn settings_envelope(
    hub: &OverlayHub,
    id: &str,
    values: serde_json::Map<String, serde_json::Value>,
) -> protocol::Envelope {
    hub.envelope(Command::Plugin(protocol::plugin::PluginCommand {
        plugin: id.to_string(),
        message: PluginMessage::Settings { values },
    }))
}

/// What a page kept, for it when it loads.
fn kept_envelope(hub: &OverlayHub, memory: &PageMemory, id: &str) -> Option<protocol::Envelope> {
    let value = memory.get(id)?;
    Some(hub.envelope(Command::Plugin(protocol::plugin::PluginCommand {
        plugin: id.to_string(),
        message: PluginMessage::Kept { value },
    })))
}

/// Sends an overlay its plugin pages again if they changed, the settings
/// of pages whose settings changed (or that are new), and what new pages
/// kept.
#[allow(clippy::too_many_arguments)]
async fn update_pages(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    hub: &OverlayHub,
    installed: &Plugins,
    store: &ConfigStore,
    files: &PluginFiles,
    memory: &PageMemory,
    media: &MediaFolders,
    link: &Option<OverlayLink>,
    plugins: &mut Vec<String>,
    sent_settings: &mut BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
) -> Result<(), axum::Error> {
    let Some(link) = link else { return Ok(()) };
    let config = store.current();
    let now = hosted_pages(installed, &config, link);
    let mut new = Vec::new();
    if now != *plugins {
        new = now.iter().filter(|id| !plugins.contains(id)).cloned().collect();
        *plugins = now;
        sent_settings.retain(|id, _| plugins.contains(id));
        let pages = hub.envelope(Command::Shell(ShellCommand::Plugins { plugins: plugins.clone() }));
        send(sender, &pages).await?;
    }
    for id in plugins.iter() {
        let Some(seen) = page_settings(installed, &config, files, id, &mut folder_as_is) else { continue };
        if sent_settings.get(id) != Some(&seen) {
            sent_settings.insert(id.clone(), seen);
            let mut pick = |kind, folder: &str| media.pick_any(kind, folder);
            let Some(values) = page_settings(installed, &config, files, id, &mut pick) else { continue };
            send(sender, &settings_envelope(hub, id, values)).await?;
        }
    }
    for id in &new {
        if let Some(kept) = kept_envelope(hub, memory, id) {
            send(sender, &kept).await?;
        }
    }
    Ok(())
}

async fn send(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    envelope: &protocol::Envelope,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(envelope).expect("envelopes serialise");
    sender.send(WsMessage::Text(json.into())).await
}

/// Identifies the overlay build being served: index.html names the hashed
/// `.js` / `.wasm` files, so it changes whenever they do. `None` if the
/// overlay hasn't been built.
fn overlay_build() -> Option<String> {
    let file = OverlayAssets::get("index.html")?;
    Some(file.metadata.sha256_hash()[..8].iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Dev only: when the overlay is rebuilt (`trunk watch`), announce the new
/// build so open overlays reload. Debug builds read the overlay from disk;
/// release builds embed it, so their build never changes while running.
fn spawn_build_watcher(hub: OverlayHub) {
    tokio::spawn(async move {
        let mut last = overlay_build();
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let current = overlay_build();
            if let Some(id) = &current
                && current != last
            {
                tracing::info!("overlay rebuilt; reloading open overlays");
                hub.send(Command::Shell(ShellCommand::Build { id: id.clone() }));
            }
            last = current;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::local_host_name;

    #[test]
    fn only_localhost_or_an_address_reaches_the_app() {
        for host in
            ["localhost", "localhost:5750", "LOCALHOST:3100", "localhost.", "127.0.0.1", "127.0.0.1:5750"]
        {
            assert!(local_host_name(host), "{host}");
        }
        for host in ["[::1]", "[::1]:5750", "192.168.1.20:5750", "10.0.0.2"] {
            assert!(local_host_name(host), "{host}");
        }
        for host in
            ["evil.example", "evil.example:5750", "localhost.evil.example", "", "[::1", "[evil]:5750", "?"]
        {
            assert!(!local_host_name(host), "{host}");
        }
    }
}
