//! `cargo feed <exe>`: the update feed for a built `lsk.exe`, made by the
//! release workflow. It writes `latest.json` (the version, the exe's size
//! and SHA-256, as `protocol::update::Feed`) and `latest.json.sig` (the
//! base64 of the ed25519 signature over `latest.json`'s exact bytes) beside
//! the exe, signed with the private key in `LSK_RELEASE_KEY`: the standard
//! base64 of the PKCS#8 v2 document `cargo keygen` printed.

use std::path::{Path, PathBuf};

use base64::Engine;
use protocol::update::Feed;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Runs `cargo feed <exe>`.
pub fn feed(args: &[String]) -> Result<(), String> {
    let [exe] = args else {
        return Err("usage: cargo feed <lsk.exe>, with the signing key in LSK_RELEASE_KEY".to_string());
    };
    let exe = PathBuf::from(exe);
    let key = std::env::var("LSK_RELEASE_KEY").map_err(|_| "LSK_RELEASE_KEY isn't set".to_string())?;
    let bytes = std::fs::read(&exe).map_err(|error| format!("{}: {error}", exe.display()))?;
    // The workspace's version, which this crate shares with the app.
    let signed = sign(&bytes, env!("CARGO_PKG_VERSION"), &key)?;
    if signed.public_key != protocol::update::PUBLIC_KEY {
        return Err("LSK_RELEASE_KEY isn't the private half of the app's PUBLIC_KEY: installed copies \
                    would refuse this release"
            .to_string());
    }
    let folder = exe.parent().unwrap_or(Path::new("."));
    for (name, contents) in
        [("latest.json", &signed.feed), ("latest.json.sig", &signed.signature.into_bytes())]
    {
        let path = folder.join(name);
        std::fs::write(&path, contents).map_err(|error| format!("{}: {error}", path.display()))?;
        println!("Wrote {}", path.display());
    }
    println!("{}", String::from_utf8_lossy(&signed.feed));
    Ok(())
}

/// A signed feed.
struct Signed {
    /// `latest.json`'s bytes.
    feed: Vec<u8>,
    /// `latest.json.sig`: the signature over them, in base64.
    signature: String,
    /// The public half of the key it was signed with, in base64, as the
    /// app has it.
    public_key: String,
}

/// The feed for version `version` of the exe `exe`, signed with `key`
/// (base64 of a PKCS#8 v2 ed25519 key).
fn sign(exe: &[u8], version: &str, key: &str) -> Result<Signed, String> {
    let base64 = base64::engine::general_purpose::STANDARD;
    let pkcs8 = base64.decode(key.trim()).map_err(|_| "LSK_RELEASE_KEY isn't base64".to_string())?;
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| "LSK_RELEASE_KEY isn't an ed25519 key from cargo keygen".to_string())?;
    let digest = ring::digest::digest(&ring::digest::SHA256, exe);
    let sha256 = digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    let feed = Feed { version: version.to_string(), size: exe.len() as u64, sha256 };
    let feed = serde_json::to_vec(&feed).map_err(|error| format!("writing latest.json: {error}"))?;
    let signature = pair.sign(&feed);
    Ok(Signed {
        feed,
        signature: base64.encode(signature.as_ref()),
        public_key: base64.encode(pair.public_key().as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ED25519, UnparsedPublicKey};

    /// What the app does with a feed it downloads (`verify` in the
    /// server's `update.rs`), with the same calls: the signature with
    /// `ring`, then the JSON.
    fn app_check(feed: &[u8], signature: &str, key: &str) -> Option<Feed> {
        let base64 = base64::engine::general_purpose::STANDARD;
        let key = base64.decode(key.trim()).ok()?;
        let signature = base64.decode(signature.trim()).ok()?;
        UnparsedPublicKey::new(&ED25519, &key).verify(feed, &signature).ok()?;
        serde_json::from_slice(feed).ok()
    }

    #[test]
    fn the_app_accepts_what_is_signed() {
        let (private, public) = crate::key_pair().unwrap();
        let signed = sign(b"abc", "0.2.0", &private).unwrap();
        assert_eq!(signed.public_key, public);
        let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            String::from_utf8(signed.feed.clone()).unwrap(),
            format!(r#"{{"version":"0.2.0","size":3,"sha256":"{sha256}"}}"#)
        );
        let feed = app_check(&signed.feed, &signed.signature, &public).expect("the app accepts it");
        assert_eq!(feed, Feed { version: "0.2.0".into(), size: 3, sha256: sha256.into() });

        let (_, other) = crate::key_pair().unwrap();
        assert!(app_check(&signed.feed, &signed.signature, &other).is_none(), "another key refuses it");
        let mut changed = signed.feed.clone();
        changed[14] = b'3'; // 0.3.0
        assert!(app_check(&changed, &signed.signature, &public).is_none(), "a changed feed is refused");
        assert!(sign(b"abc", "0.2.0", "not a key").is_err());
    }
}
