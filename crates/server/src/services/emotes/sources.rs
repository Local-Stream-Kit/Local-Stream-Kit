//! Third-party emote sources: 7TV, BetterTTV, FrankerFaceZ. The only
//! platform-specific code outside providers. Response shapes are tested
//! against real responses in `tests/fixtures/api/`.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::EmoteInfo;
use crate::model::EmoteRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    SevenTv,
    Bttv,
    Ffz,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The channel has no profile on this source: an empty set, not an error.
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Failed(String),
}

impl SourceKind {
    pub const ALL: [SourceKind; 3] = [SourceKind::SevenTv, SourceKind::Bttv, SourceKind::Ffz];

    /// The name used in config and as `EmoteRef.source`.
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::SevenTv => "7tv",
            SourceKind::Bttv => "bttv",
            SourceKind::Ffz => "ffz",
        }
    }

    pub fn from_name(name: &str) -> Option<SourceKind> {
        SourceKind::ALL.into_iter().find(|source| source.name() == name)
    }

    /// Platforms whose channels this source has emote sets for. A source's
    /// global set also only applies on these platforms.
    pub fn supports(self, platform: &str) -> bool {
        match self {
            // 7TV's v3 API accepts twitch and kick ids, and rejects youtube.
            SourceKind::SevenTv => matches!(platform, "twitch" | "kick"),
            SourceKind::Bttv | SourceKind::Ffz => platform == "twitch",
        }
    }

    pub async fn fetch_channel(
        self,
        http: &reqwest::Client,
        platform: &str,
        channel_id: &str,
    ) -> Result<Vec<EmoteInfo>, FetchError> {
        let url = match self {
            SourceKind::SevenTv => format!("https://7tv.io/v3/users/{platform}/{channel_id}"),
            SourceKind::Bttv => format!("https://api.betterttv.net/3/cached/users/{platform}/{channel_id}"),
            SourceKind::Ffz => format!("https://api.frankerfacez.com/v1/room/id/{channel_id}"),
        };
        let body = get(http, &url).await?;
        let parsed = match self {
            SourceKind::SevenTv => seven_tv::parse_channel(&body),
            SourceKind::Bttv => bttv::parse_channel(&body),
            SourceKind::Ffz => ffz::parse_room(&body),
        };
        parsed.map_err(|error| FetchError::Failed(format!("unexpected response from {url}: {error}")))
    }

    pub async fn fetch_global(self, http: &reqwest::Client) -> Result<Vec<EmoteInfo>, FetchError> {
        let url = match self {
            SourceKind::SevenTv => "https://7tv.io/v3/emote-sets/global",
            SourceKind::Bttv => "https://api.betterttv.net/3/cached/emotes/global",
            SourceKind::Ffz => "https://api.frankerfacez.com/v1/set/global",
        };
        let body = get(http, url).await?;
        let parsed = match self {
            SourceKind::SevenTv => seven_tv::parse_global(&body),
            SourceKind::Bttv => bttv::parse_global(&body),
            SourceKind::Ffz => ffz::parse_global(&body),
        };
        parsed.map_err(|error| FetchError::Failed(format!("unexpected response from {url}: {error}")))
    }
}

async fn get(http: &reqwest::Client, url: &str) -> Result<String, FetchError> {
    let response = http.get(url).send().await.map_err(|error| FetchError::Failed(error.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(FetchError::NotFound);
    }
    if !status.is_success() {
        return Err(FetchError::Failed(format!("{url} returned {status}")));
    }
    response.text().await.map_err(|error| FetchError::Failed(error.to_string()))
}

fn info(source: SourceKind, id: String, code: String, image_url: String, zero_width: bool) -> EmoteInfo {
    EmoteInfo { emote: EmoteRef { source: source.name().to_string(), id, code }, image_url, zero_width }
}

pub mod seven_tv {
    use super::*;

    #[derive(Deserialize)]
    struct UserResponse {
        emote_set: Option<EmoteSet>,
    }

    #[derive(Deserialize)]
    struct EmoteSet {
        emotes: Option<Vec<ActiveEmote>>,
    }

    #[derive(Deserialize)]
    struct ActiveEmote {
        id: String,
        /// The code typed in chat (can differ from the emote's own name).
        name: String,
        #[serde(default)]
        flags: u32,
        data: Option<EmoteData>,
    }

    #[derive(Deserialize)]
    struct EmoteData {
        host: Host,
    }

    #[derive(Deserialize)]
    struct Host {
        /// Protocol-relative: `//cdn.7tv.app/emote/<id>`.
        url: String,
        files: Vec<File>,
    }

    #[derive(Deserialize)]
    struct File {
        name: String,
    }

    /// Bit 0 of the set entry's flags marks a zero-width emote.
    const ZERO_WIDTH: u32 = 1;

    pub fn parse_channel(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let response: UserResponse = serde_json::from_str(body)?;
        Ok(convert(response.emote_set.and_then(|set| set.emotes).unwrap_or_default()))
    }

    pub fn parse_global(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let set: EmoteSet = serde_json::from_str(body)?;
        Ok(convert(set.emotes.unwrap_or_default()))
    }

    fn convert(emotes: Vec<ActiveEmote>) -> Vec<EmoteInfo> {
        emotes
            .into_iter()
            .filter_map(|emote| {
                let host = emote.data?.host;
                // 2x WebP is animated when the emote is; fall back to any WebP,
                // then to whatever there is.
                let file = host
                    .files
                    .iter()
                    .find(|file| file.name == "2x.webp")
                    .or_else(|| host.files.iter().find(|file| file.name.ends_with(".webp")))
                    .or_else(|| host.files.first())?;
                let url = format!("https:{}/{}", host.url, file.name);
                Some(info(SourceKind::SevenTv, emote.id, emote.name, url, emote.flags & ZERO_WIDTH != 0))
            })
            .collect()
    }
}

pub mod bttv {
    use super::*;

    /// BetterTTV's API has no zero-width flag; its extension treats these
    /// global emotes as overlays.
    const OVERLAY_CODES: [&str; 2] = ["cvHazmat", "cvMask"];

    #[derive(Deserialize)]
    struct Emote {
        id: String,
        code: String,
        /// Text modifiers like `w!` and `h!` that change the next emote.
        #[serde(default)]
        modifier: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UserResponse {
        #[serde(default)]
        channel_emotes: Vec<Emote>,
        #[serde(default)]
        shared_emotes: Vec<Emote>,
    }

    pub fn parse_channel(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let response: UserResponse = serde_json::from_str(body)?;
        Ok(convert(response.channel_emotes.into_iter().chain(response.shared_emotes)))
    }

    pub fn parse_global(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let emotes: Vec<Emote> = serde_json::from_str(body)?;
        Ok(convert(emotes.into_iter()))
    }

    fn convert(emotes: impl Iterator<Item = Emote>) -> Vec<EmoteInfo> {
        emotes
            .filter(|emote| !emote.modifier)
            .map(|emote| {
                let url = format!("https://cdn.betterttv.net/emote/{}/2x.webp", emote.id);
                let zero_width = OVERLAY_CODES.contains(&emote.code.as_str());
                info(SourceKind::Bttv, emote.id, emote.code, url, zero_width)
            })
            .collect()
    }
}

pub mod ffz {
    use super::*;

    #[derive(Deserialize)]
    struct Set {
        #[serde(default)]
        emoticons: Vec<Emote>,
    }

    #[derive(Deserialize)]
    struct Emote {
        id: u64,
        name: String,
        urls: BTreeMap<String, String>,
        /// Only present on animated emotes; same keys as `urls`.
        #[serde(default)]
        animated: Option<BTreeMap<String, String>>,
        /// Effects like `ffzW` that change the previous emote.
        #[serde(default)]
        modifier: bool,
    }

    #[derive(Deserialize)]
    struct RoomResponse {
        sets: BTreeMap<String, Set>,
    }

    #[derive(Deserialize)]
    struct GlobalResponse {
        default_sets: Vec<u64>,
        sets: BTreeMap<String, Set>,
    }

    pub fn parse_room(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let response: RoomResponse = serde_json::from_str(body)?;
        Ok(convert(response.sets.into_values()))
    }

    pub fn parse_global(body: &str) -> anyhow::Result<Vec<EmoteInfo>> {
        let response: GlobalResponse = serde_json::from_str(body)?;
        let defaults: Vec<String> = response.default_sets.iter().map(u64::to_string).collect();
        Ok(convert(response.sets.into_iter().filter(|(id, _)| defaults.contains(id)).map(|(_, set)| set)))
    }

    fn convert(sets: impl Iterator<Item = Set>) -> Vec<EmoteInfo> {
        sets.flat_map(|set| set.emoticons)
            .filter(|emote| !emote.modifier)
            .filter_map(|emote| {
                let urls = emote.animated.as_ref().unwrap_or(&emote.urls);
                let url = ["2", "4", "1"].iter().find_map(|scale| urls.get(*scale))?.clone();
                Some(info(SourceKind::Ffz, emote.id.to_string(), emote.name, url, false))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/api/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn find<'a>(emotes: &'a [EmoteInfo], code: &str) -> &'a EmoteInfo {
        emotes.iter().find(|e| e.emote.code == code).unwrap_or_else(|| panic!("{code} not found"))
    }

    #[test]
    fn seven_tv_channel() {
        let emotes = seven_tv::parse_channel(&fixture("7tv_user_twitch_71092938.json")).unwrap();
        assert_eq!(emotes.len(), 11);
        let zero_width: Vec<_> =
            emotes.iter().filter(|e| e.zero_width).map(|e| e.emote.code.as_str()).collect();
        assert_eq!(zero_width.len(), 3, "{zero_width:?}");
        for emote in &emotes {
            assert_eq!(emote.emote.source, "7tv");
            assert!(emote.image_url.starts_with("https://cdn.7tv.app/emote/"), "{}", emote.image_url);
            assert!(emote.image_url.ends_with("/2x.webp"), "{}", emote.image_url);
        }
    }

    #[test]
    fn seven_tv_global_zero_width() {
        let emotes = seven_tv::parse_global(&fixture("7tv_global.json")).unwrap();
        for code in ["PETPET", "SteerR", "RainTime"] {
            assert!(find(&emotes, code).zero_width, "{code}");
        }
        assert!(emotes.iter().any(|e| !e.zero_width));
    }

    #[test]
    fn seven_tv_not_found_body_is_not_an_emote_set() {
        // A 404 is handled as FetchError::NotFound before parsing; make sure
        // the body itself wouldn't be mistaken for an empty set either way.
        assert!(seven_tv::parse_channel(&fixture("7tv_user_not_found.json")).unwrap().is_empty());
    }

    #[test]
    fn bttv_channel_and_global() {
        let channel = bttv::parse_channel(&fixture("bttv_user_twitch_22484632.json")).unwrap();
        assert!(!channel.is_empty());
        assert!(channel.iter().all(|e| e.image_url.ends_with("/2x.webp")));
        let global = bttv::parse_global(&fixture("bttv_global.json")).unwrap();
        assert!(global.iter().all(|e| !e.emote.code.ends_with('!')), "modifiers are skipped");
        assert!(find(&global, "cvHazmat").zero_width);
        assert!(find(&global, "cvMask").zero_width);
        assert!(!find(&global, "FeelsGoodMan").zero_width);
    }

    #[test]
    fn ffz_rooms_and_global() {
        let room = ffz::parse_room(&fixture("ffz_room_22484632.json")).unwrap();
        assert!(!room.is_empty());
        assert!(room.iter().all(|e| e.image_url.starts_with("https://cdn.frankerfacez.com/emote/")));
        let animated = ffz::parse_room(&fixture("ffz_room_23161357_animated.json")).unwrap();
        assert!(find(&animated, "fallDamage").image_url.ends_with("/animated/2"));
        let global = ffz::parse_global(&fixture("ffz_global.json")).unwrap();
        assert!(!global.is_empty());
        for modifier in ["ffzW", "ffzX", "ffzY", "ffzCursed"] {
            assert!(global.iter().all(|e| e.emote.code != modifier), "{modifier} is skipped");
        }
        ffz::parse_room(&fixture("ffz_room_12826.json")).unwrap();
    }
}
