//! Downloads what text to speech needs, once, when the panel asks: ONNX
//! Runtime (Microsoft's release; only its library is kept), the Kokoro model
//! and its English voices (Hugging Face, `onnx-community`), and
//! OpenPhonemizer (Hugging Face, `nekwo`) for words misaki doesn't know.
//! The Hugging Face files are pinned to a revision, and every download is
//! checked against its SHA-256 before it's kept. Files go in
//! under a temporary name and are renamed when complete, so a broken
//! download is fetched again next time and finished files are kept.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use protocol::speech::{SpeechStatus, VOICES};
use sha2::{Digest, Sha256};

use super::kokoro::{Files, runtime_name};
use crate::services::status::StatusHub;

/// A month old when chosen; `ort` 2.0.0-rc.13 is built for ONNX Runtime
/// 1.28.
const RUNTIME_VERSION: &str = "1.28.2";
const KOKORO_URL: &str = "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231";
const PHONEMIZER_URL: &str =
    "https://huggingface.co/nekwo/openphonemizer-en-us-onnx/resolve/1e8cb072f1c2aa17286f235a7740b596ac7bea31";

/// The SHA-256 of each download, computed from the files at the addresses
/// above on 2026-10-14 (the GitHub release's matches the digest GitHub
/// lists, the model's the one Hugging Face lists).
const RUNTIME_SHA256: &str = "c4eedd29489d5feca21866d054638416f3655bf6b18851b3b6b85c8313e95c35";
const MODEL_SHA256: &str = "8fbea51ea711f2af382e88c833d9e288c6dc82ce5e98421ea61c058ce21a34cb";
const PHONEMIZER_SHA256: &str = "a77b17647a02cb9ac42025cfc60c8e83c70f36785183a4388f17722fb1c7dc9b";
const PHONEMIZER_DATA_SHA256: &str = "36dda6260c7e560a4d846528387fd07ababcd1012a52b530063fefadd5987db0";
/// Each voice's, by its id.
const VOICE_SHA256: &[(&str, &str)] = &[
    ("af_alloy", "c4a6b876047fd7fb472edf4ebd63cfac7c3b958a7cae7c106e8f038ca6308c45"),
    ("af_aoede", "4a004c33430762e2461eedb2013fad808ef4ab3121f5300f554476caf58d8361"),
    ("af_bella", "f69d836209b78eb8c66e75e3cda491e26ea838a3674257e9d4e5703cbaf55c8b"),
    ("af_heart", "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b"),
    ("af_jessica", "a240a5e3c15b43563d6e923bdca8ef5613a23471d9b77653694012435df23bd8"),
    ("af_kore", "9be5221b6a941c04b561959b8ff0b06e809444dcc4ab7e75a7b23606f691819e"),
    ("af_nicole", "cd2191ab31b914ed7b318416b0e4440fdf392ddad9106a060819aa600a64f59a"),
    ("af_nova", "18778272caa0d0eebaea251c35fd635f038434f9eee5e691d02a174bd328414f"),
    ("af_river", "00a2bcf82b1d86e8f19902ede58c65ccf6c0e43b44b7d74fad54e5d8933c9c30"),
    ("af_sarah", "4409fbc125afabacc615d94db5398d847006a737b0247d6892b7a9a0007a2f0a"),
    ("af_sky", "4435255c9744f3f31659e0d714ab7689bf65d9e77ec1cce060f083912614f0b9"),
    ("am_adam", "162b035ed91cfc48b6046982184c645f72edcdd1b82843347f605d7bf7b15716"),
    ("am_echo", "3968b92c3c4cd1c4416dbded36c13eaa388a90d5788d02a13e4d781f5f8cf3c3"),
    ("am_eric", "e8b5be17edd1e3636901ce7598baafe2dc8dd8ff707a0c23bf9e461add7e2832"),
    ("am_fenrir", "c27989f741f7ee34d273a39d8a595cc0837d35f5ced9a29b7cc162614616df43"),
    ("am_liam", "52403be32fd047c6a44517cb0bcd6b134f2a18baa73e70ef41651e0eab921ade"),
    ("am_michael", "1d1f21dd8da39c30705cd4c75d039d265e9bc4a2a93ed09bc9e1b1225eb95ba1"),
    ("am_onyx", "da5d135b424164916d75a68ffb4c2abce3d7d5ccc82dd1ee6cf447ce286145e6"),
    ("am_puck", "fcf73c989033e9233e0b98713eca600c8c74dcc1614b37009d5450ff4a2274a0"),
    ("am_santa", "61150cf726ab6c5ed7a99f90a304f91f5a72c00c592e89ec94e5df11c319227a"),
    ("bf_alice", "08afa6ba24da61ea5e8efa139e5aadc938d83f0a6da5a900adaf763ac1da5573"),
    ("bf_emma", "669fe0647f9dd04fcab92f1439a40eeb4c8b4ab1f82e4996fe3d918ce4a63b73"),
    ("bf_isabella", "3754352c4aaa46d17f27654ab7518d65b62ad6163a0f55a5f4330c2da2c4e94f"),
    ("bf_lily", "5e0ee32ebe64a467124976b14e69590746f1c4ce41a12b587a50c862edfea335"),
    ("bm_daniel", "6b3194bbceffb746733cbc22c8f593dd44e401a71d53895a2dca891bc595a1e8"),
    ("bm_fable", "f889083196807b4adb15e9204252165f503b8d33d3982e681c52443c49d798f1"),
    ("bm_george", "c4b235a4c1f2cd3b939fed08b899ce9385638b763f7b73a59616c4fc9bd6c9bc"),
    ("bm_lewis", "b8f671cef828c30e66fdf0b0756a76bba58f6bb3398cbbf27058642acbcedb97"),
];

fn voice_sha256(id: &str) -> anyhow::Result<&'static str> {
    VOICE_SHA256
        .iter()
        .find(|(voice, _)| *voice == id)
        .map(|(_, sha256)| *sha256)
        .ok_or_else(|| anyhow!("no SHA-256 for the voice {id}"))
}

/// Rough sizes, for one progress bar over all downloads.
const RUNTIME_BYTES: u64 = 79_000_000;
const MODEL_BYTES: u64 = 326_000_000;
const VOICE_BYTES: u64 = 523_000;
const PHONEMIZER_BYTES: u64 = 31_000_000;
const PHONEMIZER_DATA_BYTES: u64 = 9_200_000;

/// Where ONNX Runtime comes from on this system, and its library's path in
/// the archive.
fn runtime_download() -> anyhow::Result<(String, String)> {
    let name = if cfg!(all(windows, target_arch = "x86_64")) {
        format!("onnxruntime-win-x64-{RUNTIME_VERSION}")
    } else {
        bail!("text to speech can only be installed on 64-bit Windows so far");
    };
    let url =
        format!("https://github.com/microsoft/onnxruntime/releases/download/v{RUNTIME_VERSION}/{name}.zip");
    Ok((url, format!("{name}/lib/{}", runtime_name())))
}

/// Whether everything is in place.
pub fn installed(files: &Files) -> bool {
    files.runtime.is_file()
        && files.model.is_file()
        && files.phonemizer.is_file()
        && files.phonemizer_data.is_file()
        && VOICES.iter().all(|voice| files.voice(voice.id).is_file())
}

struct Progress<'a> {
    status: &'a StatusHub,
    done: u64,
    total: u64,
    last_percent: u8,
}

impl Progress<'_> {
    fn report(&mut self, step: &str, more: u64) {
        self.done += more;
        let percent = (self.done * 100 / self.total.max(1)).min(99) as u8;
        if percent != self.last_percent {
            self.last_percent = percent;
            self.status.set_speech(SpeechStatus::Installing { step: step.to_string(), percent });
        }
    }
}

pub async fn install(http: &reqwest::Client, dir: &Path, status: &StatusHub) -> anyhow::Result<()> {
    let files = Files::in_dir(dir);
    tokio::fs::create_dir_all(&files.voices).await?;
    let total = RUNTIME_BYTES
        + MODEL_BYTES
        + PHONEMIZER_BYTES
        + PHONEMIZER_DATA_BYTES
        + VOICE_BYTES * VOICES.len() as u64;
    let mut progress = Progress { status, done: 0, total, last_percent: u8::MAX };
    progress.report("Starting", 0);

    if !files.runtime.is_file() {
        let (url, inside) = runtime_download()?;
        let archive = dir.join("onnxruntime.zip");
        download(http, &url, RUNTIME_SHA256, &archive, "ONNX Runtime", &mut progress).await?;
        let runtime = files.runtime.clone();
        tokio::task::spawn_blocking(move || extract(&archive, &inside, &runtime)).await??;
    } else {
        progress.report("ONNX Runtime", RUNTIME_BYTES);
    }
    if !files.model.is_file() {
        download(
            http,
            &format!("{KOKORO_URL}/onnx/model.onnx"),
            MODEL_SHA256,
            &files.model,
            "The Kokoro model",
            &mut progress,
        )
        .await?;
    } else {
        progress.report("The Kokoro model", MODEL_BYTES);
    }
    let pronunciation = "Pronunciation of unknown words";
    if !files.phonemizer.is_file() {
        let url = format!("{PHONEMIZER_URL}/openphonemizer-en_us-fp16.onnx");
        download(http, &url, PHONEMIZER_SHA256, &files.phonemizer, pronunciation, &mut progress).await?;
    } else {
        progress.report(pronunciation, PHONEMIZER_BYTES);
    }
    if !files.phonemizer_data.is_file() {
        let url = format!("{PHONEMIZER_URL}/openphonemizer-en_us.json");
        download(http, &url, PHONEMIZER_DATA_SHA256, &files.phonemizer_data, pronunciation, &mut progress)
            .await?;
    } else {
        progress.report(pronunciation, PHONEMIZER_DATA_BYTES);
    }
    for voice in VOICES {
        let path = files.voice(voice.id);
        if !path.is_file() {
            let url = format!("{KOKORO_URL}/voices/{}.bin", voice.id);
            let sha256 = voice_sha256(voice.id)?;
            download(http, &url, sha256, &path, &format!("Voice: {}", voice.name), &mut progress).await?;
        } else {
            progress.report("Voices", VOICE_BYTES);
        }
    }
    Ok(())
}

/// Downloads to `path` (through `path.part`), if its SHA-256 is `sha256`.
async fn download(
    http: &reqwest::Client,
    url: &str,
    sha256: &str,
    path: &Path,
    step: &str,
    progress: &mut Progress<'_>,
) -> anyhow::Result<()> {
    tracing::info!(url, "downloading for text to speech");
    // The shared client times out after 20 s; these downloads take longer.
    let mut response = http
        .get(url)
        .timeout(std::time::Duration::from_secs(60 * 30))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("couldn't download {url}"))?;
    let partial = path.with_extension("part");
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut hasher = Sha256::new();
    while let Some(chunk) =
        response.chunk().await.with_context(|| format!("the download of {url} broke off"))?
    {
        hasher.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        progress.report(step, chunk.len() as u64);
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);
    if let Err(error) = check_sha256(&hasher.finalize(), sha256, url) {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error);
    }
    tokio::fs::rename(&partial, path).await?;
    Ok(())
}

fn check_sha256(digest: &[u8], expected: &str, url: &str) -> anyhow::Result<()> {
    let found: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if found != expected {
        bail!("the download of {url} isn't the expected file (SHA-256 {found}, expected {expected})");
    }
    Ok(())
}

/// Takes one file out of a zip archive, then deletes the archive.
fn extract(archive: &Path, inside: &str, to: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).context("the ONNX Runtime download isn't a zip archive")?;
    let mut entry =
        zip.by_name(inside).map_err(|_| anyhow!("{inside} isn't in the ONNX Runtime download"))?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    let partial = to.with_extension("part");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, to)?;
    drop(entry);
    drop(zip);
    let _ = std::fs::remove_file(archive);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_voice_has_a_sha256() {
        for voice in VOICES {
            assert_eq!(voice_sha256(voice.id).unwrap().len(), 64, "{}", voice.id);
        }
        assert_eq!(VOICE_SHA256.len(), VOICES.len());
    }

    #[test]
    fn a_download_must_match_its_sha256() {
        let digest = Sha256::digest(b"abc");
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(check_sha256(&digest, abc, "x").is_ok());
        let other = "0".repeat(64);
        assert!(
            check_sha256(&digest, &other, "x").unwrap_err().to_string().contains("isn't the expected file")
        );
    }
}
