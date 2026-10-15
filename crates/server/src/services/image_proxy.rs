//! Serves emote images from the overlay's own origin.
//!
//! Needed because the overlay decodes animated emotes from raw bytes, which
//! requires `fetch()`, and BetterTTV's CDN sends no CORS header. Only serves
//! emotes the registry knows, using the URL the registry holds — never a URL
//! from the request — so it isn't an open proxy.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::pipeline::now_ms;
use crate::services::emotes::EmoteRegistry;
use crate::services::image_cache::{CachedImage, ImageCache, cache_key};

/// CDN requests in flight at once.
const MAX_CONCURRENT_FETCHES: usize = 6;
const ATTEMPTS: u32 = 4;
/// The largest emote image taken from a CDN. Animated ones get big (one
/// BTTV global is ~3.1 MB at 2x), but nowhere near this.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("unknown emote")]
    UnknownEmote,
    #[error("the CDN has no such image")]
    NotFound,
    #[error("{0}")]
    Failed(String),
}

pub struct ImageProxy {
    http: reqwest::Client,
    registry: Arc<EmoteRegistry>,
    cache: Arc<ImageCache>,
    fetches: Semaphore,
}

impl ImageProxy {
    pub fn new(http: reqwest::Client, registry: Arc<EmoteRegistry>, cache: Arc<ImageCache>) -> ImageProxy {
        ImageProxy { http, registry, cache, fetches: Semaphore::new(MAX_CONCURRENT_FETCHES) }
    }

    /// Saves the cache index now and then, so last-use times survive restarts.
    pub fn start_index_saver(self: &Arc<Self>) {
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let cache = Arc::clone(&cache);
                let _ = tokio::task::spawn_blocking(move || cache.save_index()).await;
            }
        });
    }

    pub async fn get(&self, source: &str, id: &str) -> Result<CachedImage, ProxyError> {
        let key = cache_key(source, id);
        let cached = {
            let (cache, key) = (Arc::clone(&self.cache), key.clone());
            tokio::task::spawn_blocking(move || cache.get(&key, now_ms())).await.ok().flatten()
        };
        // (One cached before the type was checked is fetched again.)
        if let Some(image) = cached.filter(|image| image_type(&image.content_type, &image.bytes).is_some()) {
            return Ok(image);
        }
        let url = self.registry.image_url(source, id).ok_or(ProxyError::UnknownEmote)?;
        let image = self.fetch(&url).await?;
        let (cache, stored) = (Arc::clone(&self.cache), image.clone());
        let _ = tokio::task::spawn_blocking(move || cache.put(&key, stored, now_ms())).await;
        Ok(image)
    }

    /// Fetches with limited concurrency, retrying rate limits, server errors
    /// and network errors with exponential backoff.
    async fn fetch(&self, url: &str) -> Result<CachedImage, ProxyError> {
        let _permit = self.fetches.acquire().await.map_err(|error| ProxyError::Failed(error.to_string()))?;
        let mut delay = Duration::from_millis(500);
        let mut last_error = String::new();
        for attempt in 1..=ATTEMPTS {
            match self.http.get(url).send().await {
                Ok(response) if response.status().is_success() => match read_image(url, response).await {
                    Ok(image) => return Ok(image),
                    Err(Unread::Refused(why)) => return Err(ProxyError::Failed(why)),
                    Err(Unread::Broke(error)) => last_error = error,
                },
                Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {
                    return Err(ProxyError::NotFound);
                }
                Ok(response)
                    if response.status().is_client_error()
                        && response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS =>
                {
                    return Err(ProxyError::Failed(format!("{url} returned {}", response.status())));
                }
                Ok(response) => last_error = format!("{url} returned {}", response.status()),
                Err(error) => last_error = error.to_string(),
            }
            if attempt < ATTEMPTS {
                tracing::debug!(url, attempt, error = %last_error, "retrying emote image");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
        Err(ProxyError::Failed(last_error))
    }
}

/// Why an answer wasn't taken: for good, or worth another try.
enum Unread {
    Refused(String),
    Broke(String),
}

/// A CDN's answer, if it's an image no bigger than [`MAX_IMAGE_BYTES`].
/// It's served from the app's own origin, so it must never become a page.
async fn read_image(url: &str, mut response: reqwest::Response) -> Result<CachedImage, Unread> {
    let too_big = || Unread::Refused(format!("{url} is bigger than {} MB", MAX_IMAGE_BYTES / (1024 * 1024)));
    if response.content_length().is_some_and(|length| length > MAX_IMAGE_BYTES as u64) {
        return Err(too_big());
    }
    let declared = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| Unread::Broke(error.to_string()))? {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            return Err(too_big());
        }
        bytes.extend_from_slice(&chunk);
    }
    let content_type = image_type(&declared, &bytes)
        .ok_or_else(|| Unread::Refused(format!("{url} isn't an image (Content-Type: {declared:?})")))?;
    Ok(CachedImage { content_type, bytes: bytes.into() })
}

/// The type to serve an image as: the declared one when it's an image
/// (other than SVG, which can carry scripts), else what its first bytes
/// say it is, for a CDN that doesn't say; `None` for anything else.
fn image_type(declared: &str, bytes: &[u8]) -> Option<String> {
    let essence = declared.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    if essence.starts_with("image/") && essence != "image/svg+xml" && !essence.contains(char::is_whitespace) {
        return Some(essence);
    }
    let sniffed = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [_, _, _, _, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f', ..] => "image/avif",
        _ => return None,
    };
    Some(sniffed.to_string())
}

#[cfg(test)]
mod tests {
    use super::image_type;

    #[test]
    fn only_images_are_served() {
        assert_eq!(image_type("image/webp", b"").as_deref(), Some("image/webp"));
        assert_eq!(image_type("Image/GIF; charset=x", b"").as_deref(), Some("image/gif"));
        assert_eq!(image_type("text/html", b"<html>"), None);
        assert_eq!(image_type("image/svg+xml", b"<svg>"), None);
        assert_eq!(image_type("", b"<script>"), None);
        // A CDN that doesn't say: what the bytes are.
        assert_eq!(image_type("application/octet-stream", b"GIF89a...").as_deref(), Some("image/gif"));
        assert_eq!(image_type("", b"RIFF\0\0\0\0WEBPVP8X").as_deref(), Some("image/webp"));
        assert_eq!(image_type("text/html", b"\x89PNG\r\n").as_deref(), Some("image/png"));
    }
}
