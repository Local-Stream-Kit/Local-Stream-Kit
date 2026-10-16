//! Release builds embed `crates/overlay/dist/`, `crates/panel/dist/` and the
//! bundled `plugins/` (see `web.rs`, `panel.rs`, `plugins.rs`), but rust-embed only tells cargo about the files that
//! were there at the last build: if `dist/` was missing then, a later build
//! wouldn't make it in. Watching the whole folders fixes that.
//!
//! With the `streamdeck` feature, the Stream Deck plugin's file is built in
//! too (`streamdeck.rs`): `streamdeck/dist/com.localstreamkit.streamdeck.streamDeckPlugin`,
//! which `cargo streamdeck` and `cargo dist` make, or the file
//! `LSK_STREAMDECK_PLUGIN` names (absolute, or from the workspace's
//! folder; `cargo e2e`'s stand-in). The code gets its path as
//! `STREAMDECK_PLUGIN`, and the plugin's version, from its manifest in the
//! repo, as `STREAMDECK_VERSION`.

use std::path::Path;

fn main() {
    #[cfg(feature = "streamdeck")]
    streamdeck_plugin();
    // With debug assertions on, rust-embed reads `dist/` from disk at runtime
    // instead, so there's nothing to rebuild for.
    if std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some() {
        println!("cargo::rerun-if-changed=build.rs");
        return;
    }
    // The plugins that ship with the app.
    println!("cargo::rerun-if-changed=plugins");
    for crate_name in ["overlay", "panel"] {
        println!("cargo::rerun-if-changed=../{crate_name}/dist");
        if !Path::new(&format!("../{crate_name}/dist/index.html")).is_file() {
            println!(
                "cargo::warning=crates/{crate_name}/dist/index.html is missing, so this build serves a \"not built\" \
                 page instead of the {crate_name}. Build releases with `cargo dist`."
            );
        }
    }
}

/// Where the Stream Deck plugin's file is, for `include_bytes!`, and its
/// version. Stops the build when the file isn't there.
#[cfg(feature = "streamdeck")]
fn streamdeck_plugin() {
    println!("cargo::rerun-if-env-changed=LSK_STREAMDECK_PLUGIN");
    let crate_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let workspace = Path::new(&crate_dir).join("../..");
    let path = match std::env::var_os("LSK_STREAMDECK_PLUGIN") {
        // Relative to the workspace, as cargo is usually run from there.
        Some(path) => workspace.join(path),
        None => workspace.join("streamdeck/dist/com.localstreamkit.streamdeck.streamDeckPlugin"),
    };
    let path = std::path::absolute(&path).unwrap_or(path);
    if !path.is_file() {
        stop(&format!(
            "The streamdeck feature builds in the Stream Deck plugin's file, {}, which isn't there. \
             Make it with `cargo streamdeck` (it needs Node.js 24), or build without the feature.",
            path.display()
        ));
    }
    println!("cargo::rerun-if-changed={}", path.display());
    println!("cargo::rustc-env=STREAMDECK_PLUGIN={}", path.display());

    let manifest = workspace.join("streamdeck/com.localstreamkit.streamdeck.sdPlugin/manifest.json");
    let manifest = std::path::absolute(&manifest).unwrap_or(manifest);
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| stop(&format!("{}: {error}", manifest.display())));
    let json: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|error| stop(&format!("{}: {error}", manifest.display())));
    let version = json["Version"].as_str().unwrap_or_default();
    let numbers: Vec<&str> = version.split('.').collect();
    if numbers.len() != 4 || numbers.iter().any(|number| number.parse::<u32>().is_err()) {
        stop(&format!("{} has no Version like 0.2.0.0", manifest.display()));
    }
    println!("cargo::rustc-env=STREAMDECK_VERSION={version}");
}

#[cfg(feature = "streamdeck")]
fn stop(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
