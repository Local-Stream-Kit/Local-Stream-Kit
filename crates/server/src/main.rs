mod cards;
mod config;
mod control;
mod desktop;
mod effects;
mod emote_text;
mod logging;
mod media;
mod model;
mod panel;
mod pipeline;
mod plugins;
mod providers;
mod reload;
mod services;
mod streamdeck;
mod transformers;
mod update;
mod viewers;
mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::panel::PanelState;
use crate::pipeline::Pipeline;
use crate::plugins::Plugins;
use crate::providers::Provider;
use crate::providers::replay::{Recorder, Replay};
use crate::providers::twitch_chat::TwitchChat;
use crate::providers::twitch_eventsub::{self, TwitchEventSub};
use crate::services::Services;
use crate::services::activity::Activity;
use crate::services::config_store::ConfigStore;
use crate::services::emotes::EmoteRegistry;
use crate::services::emotes::sources::SourceKind;
use crate::services::image_cache::ImageCache;
use crate::services::image_proxy::ImageProxy;
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::Speech;
use crate::services::status::StatusHub;

/// In-memory layer in front of the disk cache.
const IMAGE_MEMORY_CACHE_BYTES: u64 = 128 * 1024 * 1024;

/// Turns what happens on a Twitch stream into effects on an OBS overlay.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// Enable `POST /debug/input`, and reload open overlays as soon as the overlay is rebuilt.
    #[arg(long)]
    debug: bool,
    /// Write every Input the pipeline receives to this file, as JSON lines.
    #[arg(long, value_name = "FILE", conflicts_with = "replay")]
    record: Option<PathBuf>,
    /// Play Inputs from a recording instead of connecting to Twitch.
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,
    /// Replay speed multiplier (2.0 = twice as fast).
    #[arg(long, default_value_t = 1.0, requires = "replay")]
    replay_speed: f64,
    /// Take channel points from the Twitch CLI's mock EventSub server
    /// (`twitch event websocket start-server`) instead of Twitch. No login.
    #[arg(
        long,
        value_name = "HOST:PORT",
        num_args = 0..=1,
        default_missing_value = "127.0.0.1:8080",
        conflicts_with = "replay"
    )]
    twitch_cli: Option<String>,
    /// Where the releases are, instead of GitHub (for tests): a debug build
    /// only checks for a new version with this.
    #[arg(long, hide = true, value_name = "URL")]
    update_from: Option<String>,
    /// The exe a downloaded version replaces, instead of this one (for
    /// tests): a debug build only replaces one with this.
    #[arg(long, hide = true, value_name = "FILE")]
    update_exe: Option<PathBuf>,
    /// The key `latest.json` must be signed with (base64), instead of the
    /// built-in one (for tests).
    #[arg(long, hide = true, value_name = "BASE64")]
    update_key: Option<String>,
    /// Log what the panel asks to open on this PC (a link, the logs
    /// folder, the Stream Deck plugin) instead of opening it (for tests).
    #[arg(long, hide = true)]
    no_open: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dir = Path::new(".");
    let log = logging::Logging::start(&dir.join(logging::FOLDER));
    let mut config = Config::load_or_create(dir)?;
    log.set_detailed(config.app.log.detailed);
    let connections = config.app.connections.clone();

    let http = reqwest::Client::builder()
        .user_agent(concat!("lsk/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;
    let sources = connections.emote_sources.iter().filter_map(|name| SourceKind::from_name(name)).collect();
    let emotes = Arc::new(EmoteRegistry::new(http.clone(), sources));
    emotes.start();
    let hub = OverlayHub::new();
    let status = StatusHub::new();
    let updater = update::Updater::new(
        http.clone(),
        update::Source::new(cli.update_from.clone(), cli.update_exe.clone(), cli.update_key.clone()),
        status.clone(),
    );
    updater.clean_up();
    let speech = Speech::start(config.speech_dir(), config.speech_cache_dir(), status.clone(), http.clone());
    let activity = Activity::open(config.dir(), hub.clone(), speech.clone());
    // Writes the queue's and the history's last changes when the app stops.
    let saving = activity.clone();
    tokio::spawn(activity.clone().run());
    let media = media::MediaFolders { pictures: config.pictures_dir(), sounds: config.sounds_dir() };
    // Not while a recording plays: it isn't chat.
    let chat_log = services::chat_log::ChatLog::open(
        config.dir(),
        &connections.twitch_channel,
        config.app.chat_log.keep_days,
        Arc::clone(&emotes),
        cli.replay.is_none(),
    );
    let services = Services {
        emotes: Arc::clone(&emotes),
        overlay: hub.clone(),
        status: status.clone(),
        speech: speech.clone(),
        activity: activity.clone(),
        media: media.clone(),
        chat_log: chat_log.clone(),
    };

    let cache_dir = config.image_cache_dir();
    let cache = ImageCache::open(
        cache_dir.clone(),
        connections.image_cache_max_mb * 1024 * 1024,
        IMAGE_MEMORY_CACHE_BYTES,
    )
    .with_context(|| format!("couldn't open the image cache in {}", cache_dir.display()))?;
    let images = Arc::new(ImageProxy::new(http.clone(), Arc::clone(&emotes), Arc::new(cache)));
    images.start_index_saver();

    let plugins = Arc::new(Plugins::load(&config.plugins_dir(), config.connected_plugins_path()));
    // A file setting's one file from before 2026-10-14 is now one of its
    // looks, picked where none was.
    let moved = plugins::files::move_old_files(&config.plugin_data_dir(), &plugins.catalog());
    if !moved.is_empty() {
        for (plugin, key) in moved {
            let entry = config.app.plugins.entry(plugin).or_default();
            let settings = match entry.presets.first_mut() {
                Some(preset) => &mut preset.settings,
                None => &mut entry.settings,
            };
            settings.entry(key).or_insert_with(|| protocol::plugin::MOVED_LOOK.into());
        }
        config.save()?;
    }
    let pipeline = Pipeline::new(&config.app, &plugins.catalog());
    for command in pipeline.module_configs() {
        hub.set_module_config(command);
    }

    let (inputs, receiver) = mpsc::channel(1024);
    // The chat channel, as configured: chat follows changes while running.
    let (chat_channel, channel_updates) = watch::channel(connections.twitch_channel.clone());
    // What the Twitch login may do (Status → Twitch's ticks).
    let (twitch_permissions, permission_updates) = watch::channel(connections.twitch_permissions);
    let mut providers: Vec<Box<dyn Provider>> = Vec::new();
    let mut login = None;
    let mut moderation = None;
    if let Some(path) = &cli.replay {
        providers.push(Box::new(Replay::new(path.clone(), cli.replay_speed)));
    } else {
        let server = match &cli.twitch_cli {
            Some(address) => twitch_eventsub::Server::TwitchCli { address: address.clone() },
            // The broadcaster's login, waiting until there is one (logging
            // in from the Status page is what turns these on).
            None => twitch_eventsub::Server::Twitch {
                tokens_path: config.twitch_tokens_path(),
                chat_channel: channel_updates.clone(),
                allowed: permission_updates.clone(),
            },
        };
        providers.push(Box::new(TwitchChat::new(channel_updates.clone())));
        let (commands, receiver) = mpsc::channel(8);
        login = Some(commands);
        let eventsub = TwitchEventSub::new(http.clone(), server, receiver);
        moderation = eventsub.moderation();
        providers.push(Box::new(eventsub));
    }
    for provider in providers {
        provider.spawn(inputs.clone(), services.clone());
    }
    let recorder = cli.record.as_deref().map(Recorder::create).transpose()?;
    let (pipeline_updates, updates) = mpsc::channel(4);
    let store = ConfigStore::new(&config, pipeline_updates, plugins.catalog(), hub.clone(), status.clone());
    reload::spawn(Arc::clone(&store));
    updater.start(store.subscribe());
    {
        let mut changes = store.subscribe();
        let chat_log = chat_log.clone();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                chat_log.set_keep_days(changes.borrow().chat_log.keep_days);
                let permissions = changes.borrow().connections.twitch_permissions;
                twitch_permissions.send_if_modified(|current| {
                    let changed = *current != permissions;
                    *current = permissions;
                    changed
                });
                let channel = changes.borrow_and_update().connections.twitch_channel.clone();
                chat_channel.send_if_modified(|current| {
                    let changed = *current != channel;
                    *current = channel;
                    changed
                });
            }
        });
    }
    let (tests, test_receiver) = mpsc::channel(64);
    let (replays, replay_receiver) = mpsc::channel(64);
    let requests = pipeline::Requests {
        tests: test_receiver,
        replays: replay_receiver,
        resumes: activity.resumes(),
        updates,
    };
    tokio::spawn(pipeline::run(pipeline, receiver, requests, services, recorder));
    let files = plugins::files::PluginFiles::open(config.plugin_data_dir(), Arc::clone(&plugins));
    files.watch();
    let secrets = plugins::secrets::PluginSecrets::open(config.dir(), Arc::clone(&plugins));
    let options = plugins::options::PluginOptions::default();
    let tabs = plugins::tabs::PluginTabs::default();
    plugins::server::start(
        &plugins,
        plugins::server::Deps {
            hub: hub.clone(),
            inputs: inputs.clone(),
            status: status.clone(),
            data_dir: config.plugin_data_dir(),
            runtime: tokio::runtime::Handle::current(),
            config: store.subscribe(),
            app_port: connections.overlay_port,
            speech: speech.clone(),
            files: files.clone(),
            secrets: secrets.clone(),
            options: options.clone(),
            activity: activity.clone(),
            media: media.clone(),
            tabs: tabs.clone(),
        },
    );

    {
        let log = log.clone();
        let status = status.clone();
        let mut lines = log.subscribe();
        let mut changes = store.subscribe();
        status.set_log(log.status());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = lines.changed() => {
                        if changed.is_err() { break }
                        status.set_log(log.status());
                    }
                    changed = changes.changed() => {
                        if changed.is_err() { break }
                        log.set_detailed(changes.borrow_and_update().log.detailed);
                    }
                }
            }
        });
    }
    let control = control::ControlKeys::open(config.dir())?;
    status.set_control_key(control.key());
    let panel = PanelState {
        store,
        status,
        emotes,
        inputs: inputs.clone(),
        tests,
        login,
        replays,
        activity,
        media: media.clone(),
        speech,
        control,
        plugins,
        memory: crate::plugins::memory::PageMemory::open(config.plugin_data_dir()),
        files,
        secrets,
        options,
        tabs,
        hub: hub.clone(),
        log,
        speech_previews: Default::default(),
        updates: updater,
        desktop: desktop::Desktop::new(cli.no_open),
        cache: config.dir().join("cache"),
        chat_log,
        moderation,
    };
    let state = web::AppState { hub, images, debug_inputs: cli.debug.then(|| inputs.clone()), panel };
    let result = tokio::select! {
        result = web::serve(connections.overlay_port, state) => result,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
            Ok(())
        }
    };
    saving.flush();
    result
}
