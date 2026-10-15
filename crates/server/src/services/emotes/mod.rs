//! The emote registry: every emote that can be shown, per channel, and how to
//! get its image. Platform-neutral; it gets emotes from providers (the
//! platform's own emotes: listed by its API when there's a login, and as
//! they're seen in chat) and from third-party sources.

pub mod sources;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::watch;

use crate::model::{EmoteRef, Input, InputKind};
use crate::pipeline::RandomSource;
use sources::{FetchError, SourceKind};

/// How often third-party emote sets are reloaded.
const REFRESH: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmoteInfo {
    pub emote: EmoteRef,
    /// The CDN image the image proxy serves for this emote (animated, 2x).
    pub image_url: String,
    /// Drawn on top of the emote before it.
    pub zero_width: bool,
}

/// `(platform, channel id)`.
type ChannelKey = (String, String);

/// A chat badge's title and image, from the platform's API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeInfo {
    pub set: String,
    pub version: String,
    /// For a person: "Moderator", "6-Month Subscriber".
    pub title: String,
    pub image_url: String,
}

/// A badge as the chat display shows it: its title and where its image is
/// served (through the image proxy, like emotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownBadge {
    pub title: String,
    pub image: protocol::EmoteImage,
}

#[derive(Default)]
struct State {
    /// Source → global set.
    global: HashMap<SourceKind, Vec<EmoteInfo>>,
    /// Channel → source → that source's channel set (last good one).
    channel: HashMap<ChannelKey, HashMap<SourceKind, Vec<EmoteInfo>>>,
    /// The platform's own emotes seen in each channel this session.
    native: HashMap<ChannelKey, Vec<EmoteInfo>>,
    /// The platform's own global emotes, from its API (needs a login).
    platform_global: HashMap<String, Vec<EmoteInfo>>,
    /// Each channel's own emotes (sub, follower, bits emotes), from the
    /// platform's API.
    platform_channel: HashMap<ChannelKey, Vec<EmoteInfo>>,
    /// The words that cheer in each channel ("cheer", "biblethump", the
    /// channel's own), lowercase, from the platform's API (needs a login).
    cheer_prefixes: HashMap<ChannelKey, Vec<String>>,
    /// Code → emote for each channel, with precedence applied. Rebuilt when
    /// sets change.
    by_code: HashMap<ChannelKey, HashMap<String, EmoteInfo>>,
    /// Same, for platforms without a channel entry yet: globals only.
    global_by_code: HashMap<String, HashMap<String, EmoteInfo>>,
    /// `(source, id)` → image URL, for the image proxy.
    images: HashMap<(String, String), String>,
    /// Chat badges by `(platform, channel id)`, `""` for the platform's
    /// global ones, then by `(set, version)`.
    badges: HashMap<ChannelKey, HashMap<(String, String), BadgeInfo>>,
    /// Channels whose sets are kept fresh, with the generation of the loop
    /// doing it (a channel left and joined again gets a new loop).
    watched: HashMap<ChannelKey, u64>,
    next_generation: u64,
}

pub struct EmoteRegistry {
    http: Option<reqwest::Client>,
    /// Third-party sources in precedence order.
    sources: Vec<SourceKind>,
    state: RwLock<State>,
    /// Bumped to load every emote set again now ("Refresh emotes").
    resync: watch::Sender<u64>,
}

impl EmoteRegistry {
    pub fn new(http: reqwest::Client, sources: Vec<SourceKind>) -> EmoteRegistry {
        EmoteRegistry {
            http: Some(http),
            sources,
            state: RwLock::new(State::default()),
            resync: watch::Sender::new(0),
        }
    }

    /// A registry that never fetches anything.
    #[cfg(test)]
    pub fn for_tests() -> EmoteRegistry {
        EmoteRegistry {
            http: None,
            sources: SourceKind::ALL.to_vec(),
            state: RwLock::new(State::default()),
            resync: watch::Sender::new(0),
        }
    }

    /// Loads every emote set again now, instead of at the next refresh:
    /// the third-party ones and, with a login, Twitch's own.
    pub fn resync(&self) {
        self.resync.send_modify(|count| *count += 1);
    }

    /// Changes when `resync` is asked for.
    pub fn resyncs(&self) -> watch::Receiver<u64> {
        self.resync.subscribe()
    }

    /// Starts the background loop that keeps global sets fresh.
    pub fn start(self: &Arc<Self>) {
        let Some(http) = self.http.clone() else { return };
        let registry = Arc::clone(self);
        let mut resyncs = self.resyncs();
        tokio::spawn(async move {
            loop {
                for &source in &registry.sources {
                    match source.fetch_global(&http).await {
                        Ok(emotes) => {
                            tracing::info!(
                                source = source.name(),
                                count = emotes.len(),
                                "loaded global emotes"
                            );
                            registry.set_global(source, emotes);
                        }
                        Err(error) => {
                            tracing::warn!(source = source.name(), %error, "couldn't load global emotes; keeping the previous set")
                        }
                    }
                }
                tokio::select! {
                    Ok(()) = resyncs.changed() => {}
                    _ = tokio::time::sleep(REFRESH) => {}
                }
            }
        });
    }

    /// Lets the registry react to inputs: a joined channel gets its
    /// third-party sets loaded (in the background).
    pub fn observe(self: &Arc<Self>, input: &Input) {
        if matches!(input.kind, InputKind::ChannelJoined) {
            self.watch_channel(&input.origin.platform, &input.origin.channel.id);
        }
    }

    fn watch_channel(self: &Arc<Self>, platform: &str, channel_id: &str) {
        let key = (platform.to_string(), channel_id.to_string());
        let generation = {
            let mut state = self.state.write().unwrap();
            if state.watched.contains_key(&key) {
                return;
            }
            state.next_generation += 1;
            let generation = state.next_generation;
            state.watched.insert(key.clone(), generation);
            generation
        };
        let Some(http) = self.http.clone() else { return };
        let registry = Arc::clone(self);
        let mut resyncs = self.resyncs();
        tokio::spawn(async move {
            loop {
                // Stop once chat has left the channel.
                if registry.state.read().unwrap().watched.get(&key) != Some(&generation) {
                    return;
                }
                let (platform, channel_id) = (&key.0, &key.1);
                for &source in registry.sources.iter().filter(|source| source.supports(platform)) {
                    match source.fetch_channel(&http, platform, channel_id).await {
                        Ok(emotes) => {
                            tracing::info!(source = source.name(), %platform, %channel_id, count = emotes.len(), "loaded channel emotes");
                            registry.set_channel(platform, channel_id, source, emotes);
                        }
                        Err(FetchError::NotFound) => {
                            tracing::info!(source = source.name(), %platform, %channel_id, "channel has no emotes on this source");
                            registry.set_channel(platform, channel_id, source, Vec::new());
                        }
                        Err(error) => {
                            tracing::warn!(source = source.name(), %platform, %channel_id, %error, "couldn't load channel emotes; keeping the previous set")
                        }
                    }
                }
                tokio::select! {
                    Ok(()) = resyncs.changed() => {}
                    _ = tokio::time::sleep(REFRESH) => {}
                }
            }
        });
    }

    fn set_global(&self, source: SourceKind, emotes: Vec<EmoteInfo>) {
        let mut state = self.state.write().unwrap();
        remember_images(&mut state, &emotes);
        state.global.insert(source, emotes);
        let keys: Vec<ChannelKey> = state.by_code.keys().cloned().collect();
        for key in keys {
            self.rebuild_channel(&mut state, &key);
        }
        let platforms: Vec<String> = state.global_by_code.keys().cloned().collect();
        for platform in platforms {
            self.rebuild_global(&mut state, &platform);
        }
    }

    fn set_channel(&self, platform: &str, channel_id: &str, source: SourceKind, emotes: Vec<EmoteInfo>) {
        let mut state = self.state.write().unwrap();
        remember_images(&mut state, &emotes);
        let key = (platform.to_string(), channel_id.to_string());
        state.channel.entry(key.clone()).or_default().insert(source, emotes);
        self.rebuild_channel(&mut state, &key);
    }

    /// Emotes known for a channel, per third-party source (in source
    /// order, then the platform's own when its API listed them), and the
    /// global emotes of all sources together.
    pub fn counts(&self, platform: &str, channel_id: &str) -> (Vec<(String, usize)>, usize) {
        let state = self.state.read().unwrap();
        let key = (platform.to_string(), channel_id.to_string());
        let channel = state.channel.get(&key);
        let mut per_source: Vec<(String, usize)> = self
            .sources
            .iter()
            .map(|source| {
                let count = channel.and_then(|sets| sets.get(source)).map_or(0, Vec::len);
                (source.name().to_string(), count)
            })
            .collect();
        let platform_global = state.platform_global.get(platform);
        let platform_channel = state.platform_channel.get(&key);
        if platform_global.is_some() || platform_channel.is_some() {
            per_source.push((platform.to_string(), platform_channel.map_or(0, Vec::len)));
        }
        let global =
            self.sources.iter().map(|source| state.global.get(source).map_or(0, Vec::len)).sum::<usize>()
                + platform_global.map_or(0, Vec::len);
        (per_source, global)
    }

    /// The platform's own global emotes, from its API.
    pub fn set_platform_global(&self, platform: &str, emotes: Vec<EmoteInfo>) {
        let mut state = self.state.write().unwrap();
        remember_images(&mut state, &emotes);
        state.platform_global.insert(platform.to_string(), emotes);
    }

    /// A channel's own emotes, from the platform's API.
    pub fn set_platform_channel(&self, platform: &str, channel_id: &str, emotes: Vec<EmoteInfo>) {
        let mut state = self.state.write().unwrap();
        remember_images(&mut state, &emotes);
        state.platform_channel.insert((platform.to_string(), channel_id.to_string()), emotes);
    }

    /// The prefixes that cheer in a channel (`Cheer` in `Cheer100`), from the
    /// platform's API.
    pub fn set_cheer_prefixes(&self, platform: &str, channel_id: &str, prefixes: Vec<String>) {
        let prefixes = prefixes.into_iter().map(|prefix| prefix.to_lowercase()).collect();
        let mut state = self.state.write().unwrap();
        state.cheer_prefixes.insert((platform.to_string(), channel_id.to_string()), prefixes);
    }

    /// A platform's chat badges: a channel's own (`Some(id)`), or its global
    /// ones. They replace the ones known before.
    pub fn set_badges(&self, platform: &str, channel_id: Option<&str>, badges: Vec<BadgeInfo>) {
        let channel = channel_id.unwrap_or_default().to_string();
        let mut state = self.state.write().unwrap();
        let mut known = HashMap::new();
        for badge in badges {
            let id = badge_image_id(&channel, &badge.set, &badge.version);
            state.images.insert((badge_source(platform), id), badge.image_url.clone());
            known.insert((badge.set.clone(), badge.version.clone()), badge);
        }
        state.badges.insert((platform.to_string(), channel), known);
    }

    /// A badge's title and image: the channel's own (a sub badge), else the
    /// platform's global one. `None` until they've loaded (they need a
    /// login).
    pub fn badge(&self, platform: &str, channel_id: &str, set: &str, version: &str) -> Option<KnownBadge> {
        let state = self.state.read().unwrap();
        let key = (set.to_string(), version.to_string());
        [channel_id, ""].into_iter().find_map(|channel| {
            let badge = state.badges.get(&(platform.to_string(), channel.to_string()))?.get(&key)?;
            Some(KnownBadge {
                title: badge.title.clone(),
                image: protocol::EmoteImage {
                    source: badge_source(platform),
                    id: badge_image_id(channel, set, version),
                },
            })
        })
    }

    /// Whether the channel's cheer prefixes have loaded.
    pub fn knows_cheers(&self, platform: &str, channel_id: &str) -> bool {
        let state = self.state.read().unwrap();
        state.cheer_prefixes.contains_key(&(platform.to_string(), channel_id.to_string()))
    }

    /// Whether `word` is a cheer in the channel: one of its prefixes and an
    /// amount, any case ("Cheer100", "cheer100", "BibleThump5").
    pub fn is_cheer(&self, platform: &str, channel_id: &str, word: &str) -> bool {
        let prefix = word.trim_end_matches(|c: char| c.is_ascii_digit());
        if prefix.len() == word.len() || prefix.is_empty() {
            return false;
        }
        let state = self.state.read().unwrap();
        state
            .cheer_prefixes
            .get(&(platform.to_string(), channel_id.to_string()))
            .is_some_and(|prefixes| prefixes.iter().any(|known| known.eq_ignore_ascii_case(prefix)))
    }

    /// Ids of the channels on a platform whose emotes are being kept.
    pub fn watched(&self, platform: &str) -> Vec<String> {
        let state = self.state.read().unwrap();
        state.watched.keys().filter(|(p, _)| p == platform).map(|(_, id)| id.clone()).collect()
    }

    /// Chat left the channel: stop refreshing its sets. What's loaded stays
    /// (for a channel joined again, until its sets reload).
    pub fn unwatch(&self, platform: &str, channel_id: &str) {
        self.state.write().unwrap().watched.remove(&(platform.to_string(), channel_id.to_string()));
    }

    /// Every emote code usable in a channel, once each (the one that
    /// `lookup_any_code` finds), sorted case-insensitively: for completing
    /// codes in the control panel.
    pub fn known(&self, platform: &str, channel_id: &str) -> Vec<EmoteInfo> {
        // Make sure the globals-only map exists for channels not loaded yet.
        let _ = self.lookup_code(platform, channel_id, "");
        let state = self.state.read().unwrap();
        let key = (platform.to_string(), channel_id.to_string());
        let mut by_code: HashMap<&str, &EmoteInfo> = HashMap::new();
        let third_party = state.by_code.get(&key).or_else(|| state.global_by_code.get(platform));
        let layers =
            [state.platform_channel.get(&key), state.platform_global.get(platform), state.native.get(&key)];
        for emote in
            third_party.into_iter().flat_map(|map| map.values()).chain(layers.into_iter().flatten().flatten())
        {
            by_code.entry(emote.emote.code.as_str()).or_insert(emote);
        }
        let mut known: Vec<EmoteInfo> = by_code.into_values().cloned().collect();
        known.sort_by(|a, b| {
            a.emote
                .code
                .to_lowercase()
                .cmp(&b.emote.code.to_lowercase())
                .then_with(|| a.emote.code.cmp(&b.emote.code))
        });
        known
    }

    /// Test helper: set one source's channel set directly.
    #[cfg(test)]
    pub fn set_channel_emotes(&self, platform: &str, channel_id: &str, source: &str, emotes: Vec<EmoteInfo>) {
        let source = SourceKind::from_name(source).expect("known source");
        self.set_channel(platform, channel_id, source, emotes);
    }

    /// Channel sets first, then global sets, each in source order; the first
    /// emote with a code wins.
    fn rebuild_channel(&self, state: &mut State, key: &ChannelKey) {
        let mut map = HashMap::new();
        if let Some(sets) = state.channel.get(key) {
            for source in &self.sources {
                for emote in sets.get(source).into_iter().flatten() {
                    map.entry(emote.emote.code.clone()).or_insert_with(|| emote.clone());
                }
            }
        }
        self.add_globals(state, &key.0, &mut map);
        state.by_code.insert(key.clone(), map);
    }

    fn rebuild_global(&self, state: &mut State, platform: &str) {
        let mut map = HashMap::new();
        self.add_globals(state, platform, &mut map);
        state.global_by_code.insert(platform.to_string(), map);
    }

    fn add_globals(&self, state: &State, platform: &str, map: &mut HashMap<String, EmoteInfo>) {
        for source in self.sources.iter().filter(|source| source.supports(platform)) {
            for emote in state.global.get(source).into_iter().flatten() {
                map.entry(emote.emote.code.clone()).or_insert_with(|| emote.clone());
            }
        }
    }

    /// A third-party emote the channel can use, by its exact code.
    pub fn lookup_code(&self, platform: &str, channel_id: &str, code: &str) -> Option<EmoteInfo> {
        let state = self.state.read().unwrap();
        let key = (platform.to_string(), channel_id.to_string());
        if let Some(map) = state.by_code.get(&key) {
            return map.get(code).cloned();
        }
        if let Some(map) = state.global_by_code.get(platform) {
            return map.get(code).cloned();
        }
        drop(state);
        // First lookup for a platform with no channel loaded yet.
        let mut state = self.state.write().unwrap();
        self.rebuild_global(&mut state, platform);
        state.global_by_code.get(platform).and_then(|map| map.get(code)).cloned()
    }

    /// Like `lookup_code`, but also finds the platform's own emotes: the
    /// channel's and the global ones its API listed, and those seen so far
    /// in the channel.
    pub fn lookup_any_code(&self, platform: &str, channel_id: &str, code: &str) -> Option<EmoteInfo> {
        self.lookup_code(platform, channel_id, code).or_else(|| {
            let state = self.state.read().unwrap();
            let key = (platform.to_string(), channel_id.to_string());
            [state.platform_channel.get(&key), state.platform_global.get(platform), state.native.get(&key)]
                .into_iter()
                .flatten()
                .flatten()
                .find(|emote| emote.emote.code == code)
                .cloned()
        })
    }

    /// Records one of the platform's own emotes, seen in a channel.
    pub fn register_native(&self, platform: &str, channel_id: &str, info: EmoteInfo) {
        let key = (platform.to_string(), channel_id.to_string());
        {
            let state = self.state.read().unwrap();
            if state.native.get(&key).is_some_and(|seen| seen.iter().any(|e| e.emote.id == info.emote.id)) {
                return;
            }
        }
        let mut state = self.state.write().unwrap();
        remember_images(&mut state, std::slice::from_ref(&info));
        let seen = state.native.entry(key).or_default();
        if !seen.iter().any(|e| e.emote.id == info.emote.id) {
            seen.push(info);
        }
    }

    /// `count` random emotes the channel can use (with repeats), never
    /// zero-width ones on their own. Includes the channel's own platform
    /// emotes and the platform emotes seen so far this session, but not all
    /// of the platform's globals (Twitch's include dozens of old smileys).
    pub fn random(
        &self,
        platform: &str,
        channel_id: &str,
        count: u32,
        rng: &mut dyn RandomSource,
    ) -> Vec<EmoteInfo> {
        // Make sure the globals-only map exists for channels not loaded yet.
        let _ = self.lookup_code(platform, channel_id, "");
        let state = self.state.read().unwrap();
        let key = (platform.to_string(), channel_id.to_string());
        let third_party = state.by_code.get(&key).or_else(|| state.global_by_code.get(platform));
        let mut pool: Vec<&EmoteInfo> = third_party.into_iter().flat_map(|map| map.values()).collect();
        pool.extend(state.native.get(&key).into_iter().flatten());
        pool.extend(state.platform_channel.get(&key).into_iter().flatten());
        pool.retain(|emote| !emote.zero_width);
        // HashMap order isn't stable; sort so a seeded RNG gives repeatable picks.
        pool.sort_by(|a, b| (&a.emote.source, &a.emote.id).cmp(&(&b.emote.source, &b.emote.id)));
        pool.dedup_by(|a, b| a.emote == b.emote);
        if pool.is_empty() {
            return Vec::new();
        }
        (0..count).map(|_| pool[rng.index(pool.len())].clone()).collect()
    }

    /// The image URL for an emote the registry knows about.
    pub fn image_url(&self, source: &str, id: &str) -> Option<String> {
        self.state.read().unwrap().images.get(&(source.to_string(), id.to_string())).cloned()
    }
}

/// Badges are served by the image proxy as emotes from this source.
fn badge_source(platform: &str) -> String {
    format!("{platform}-badge")
}

fn badge_image_id(channel: &str, set: &str, version: &str) -> String {
    let channel = if channel.is_empty() { "global" } else { channel };
    format!("{channel}/{set}/{version}")
}

fn remember_images(state: &mut State, emotes: &[EmoteInfo]) {
    for emote in emotes {
        state.images.insert((emote.emote.source.clone(), emote.emote.id.clone()), emote.image_url.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::pipeline::testing::FixedRandom;

    fn info(source: &str, code: &str, zero_width: bool) -> EmoteInfo {
        EmoteInfo {
            emote: EmoteRef { source: source.into(), id: format!("{source}-{code}"), code: code.into() },
            image_url: format!("https://{source}.example/{code}"),
            zero_width,
        }
    }

    #[test]
    fn resync_wakes_whoever_listens() {
        let registry = EmoteRegistry::for_tests();
        let mut resyncs = registry.resyncs();
        assert!(!resyncs.has_changed().unwrap());
        registry.resync();
        assert!(resyncs.has_changed().unwrap());
        resyncs.mark_unchanged();
        assert!(!resyncs.has_changed().unwrap());
    }

    #[test]
    fn precedence_channel_before_global_then_source_order() {
        let registry = EmoteRegistry::for_tests();
        registry.set_global(SourceKind::SevenTv, vec![info("7tv", "EZ", false), info("7tv", "Same", false)]);
        registry.set_channel_emotes("twitch", "1", "bttv", vec![info("bttv", "Same", false)]);
        registry.set_channel_emotes("twitch", "1", "7tv", vec![info("7tv", "Mine", false)]);
        registry.set_channel_emotes("twitch", "1", "ffz", vec![info("ffz", "Mine", false)]);
        let source = |code| registry.lookup_code("twitch", "1", code).map(|e| e.emote.source);
        assert_eq!(source("Same").as_deref(), Some("bttv"), "channel sets beat global sets");
        assert_eq!(source("Mine").as_deref(), Some("7tv"), "7tv comes before ffz");
        assert_eq!(source("EZ").as_deref(), Some("7tv"), "globals still apply");
        assert_eq!(source("Nope"), None);
        // A channel that hasn't loaded yet still sees globals.
        assert_eq!(registry.lookup_code("twitch", "2", "EZ").map(|e| e.emote.code).as_deref(), Some("EZ"));
        // Globals only apply on platforms the source supports.
        assert_eq!(registry.lookup_code("youtube", "x", "EZ"), None);
    }

    #[test]
    fn random_skips_zero_width_and_includes_native() {
        let registry = EmoteRegistry::for_tests();
        registry.set_channel_emotes(
            "twitch",
            "1",
            "7tv",
            vec![info("7tv", "RainTime", true), info("7tv", "PogU", false)],
        );
        registry.register_native("twitch", "1", info("twitch", "Kappa", false));
        let picks = registry.random("twitch", "1", 20, &mut FixedRandom::new(&[0.0, 0.5, 0.99]));
        let codes: HashSet<_> = picks.iter().map(|e| e.emote.code.as_str()).collect();
        assert_eq!(codes, HashSet::from(["PogU", "Kappa"]));
        assert_eq!(
            registry.image_url("twitch", "twitch-Kappa").as_deref(),
            Some("https://twitch.example/Kappa")
        );
        assert_eq!(
            registry.lookup_any_code("twitch", "1", "Kappa").map(|e| e.emote.source).as_deref(),
            Some("twitch")
        );
    }

    #[test]
    fn channels_left_are_unwatched() {
        let registry = Arc::new(EmoteRegistry::for_tests());
        registry.watch_channel("twitch", "1");
        registry.watch_channel("twitch", "2");
        let mut watched = registry.watched("twitch");
        watched.sort();
        assert_eq!(watched, ["1", "2"]);
        registry.unwatch("twitch", "1");
        assert_eq!(registry.watched("twitch"), ["2"]);
        // Joined again: a new generation, so an old refresh loop stops.
        registry.watch_channel("twitch", "1");
        let state = registry.state.read().unwrap();
        assert!(
            state.watched[&("twitch".to_string(), "1".to_string())]
                > state.watched[&("twitch".to_string(), "2".to_string())]
        );
    }

    #[test]
    fn platform_emotes_from_the_api() {
        let registry = EmoteRegistry::for_tests();
        registry.set_channel_emotes("twitch", "1", "7tv", vec![info("7tv", "PogU", false)]);
        assert_eq!(registry.lookup_any_code("twitch", "1", "LUL"), None, "not seen, not listed");
        registry
            .set_platform_global("twitch", vec![info("twitch", "LUL", false), info("twitch", "PogU", false)]);
        registry.set_platform_channel("twitch", "1", vec![info("twitch", "marrHi", false)]);
        let source = |code| registry.lookup_any_code("twitch", "1", code).map(|e| e.emote.source);
        assert_eq!(source("LUL").as_deref(), Some("twitch"));
        assert_eq!(source("marrHi").as_deref(), Some("twitch"));
        assert_eq!(source("PogU").as_deref(), Some("7tv"), "third-party codes still come first");
        assert_eq!(
            registry.lookup_any_code("twitch", "2", "marrHi"),
            None,
            "channel emotes stay in their channel"
        );

        let codes: Vec<String> = registry.known("twitch", "1").into_iter().map(|e| e.emote.code).collect();
        assert_eq!(codes, ["LUL", "marrHi", "PogU"], "each code once, sorted ignoring case");

        let picks = registry.random("twitch", "1", 20, &mut FixedRandom::new(&[0.0, 0.99]));
        let picked: HashSet<_> = picks.iter().map(|e| e.emote.code.as_str()).collect();
        assert_eq!(picked, HashSet::from(["PogU", "marrHi"]), "random skips the platform's globals");

        let (per_source, global) = registry.counts("twitch", "1");
        assert_eq!(per_source.last(), Some(&("twitch".to_string(), 1)));
        assert_eq!(global, 2);
    }

    #[test]
    fn badges_channel_first_then_global() {
        let registry = EmoteRegistry::for_tests();
        let badge = |set: &str, version: &str, title: &str| BadgeInfo {
            set: set.into(),
            version: version.into(),
            title: title.into(),
            image_url: format!("https://badges.example/{title}"),
        };
        assert_eq!(registry.badge("twitch", "1", "subscriber", "12"), None, "nothing loaded yet");
        registry.set_badges(
            "twitch",
            None,
            vec![badge("subscriber", "12", "Global Sub"), badge("moderator", "1", "Moderator")],
        );
        registry.set_badges("twitch", Some("1"), vec![badge("subscriber", "12", "1-Year Sub")]);
        let sub = registry.badge("twitch", "1", "subscriber", "12").unwrap();
        assert_eq!(sub.title, "1-Year Sub");
        assert_eq!(sub.image.path(), "/emote/twitch-badge/1%2Fsubscriber%2F12");
        assert_eq!(
            registry.image_url(&sub.image.source, &sub.image.id).as_deref(),
            Some("https://badges.example/1-Year Sub")
        );
        assert_eq!(registry.badge("twitch", "2", "subscriber", "12").unwrap().title, "Global Sub");
        assert_eq!(registry.badge("twitch", "1", "moderator", "1").unwrap().title, "Moderator");
        assert_eq!(registry.badge("youtube", "1", "moderator", "1"), None);
    }
}
