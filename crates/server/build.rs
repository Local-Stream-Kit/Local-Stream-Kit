//! Release builds embed `crates/overlay/dist/`, `crates/panel/dist/` and the
//! bundled `plugins/` (see `web.rs`, `panel.rs`, `plugins.rs`), but rust-embed only tells cargo about the files that
//! were there at the last build: if `dist/` was missing then, a later build
//! wouldn't make it in. Watching the whole folders fixes that.

fn main() {
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
        if !std::path::Path::new(&format!("../{crate_name}/dist/index.html")).is_file() {
            println!(
                "cargo::warning=crates/{crate_name}/dist/index.html is missing, so this build serves a \"not built\" \
                 page instead of the {crate_name}. Build releases with `cargo dist`."
            );
        }
    }
}
