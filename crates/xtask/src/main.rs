//! Build tasks that take more than one command, run through the aliases in
//! `.cargo/config.toml`:
//!
//! - `cargo dist`: the release overlay, control panel and built-in plugins'
//!   pages and server code, then the server that embeds them.
//! - `cargo e2e [--rebuild-pages] [filter]`: the control panel's
//!   end-to-end tests (`crates/e2e`), after building the panel and the
//!   server they use.
//! - `cargo streamdeck`: the Stream Deck plugin (`streamdeck/`, needs
//!   Node.js 24), tested and packed into a `.streamDeckPlugin` file.
//! - `cargo plugin <folder>`: a plugin's server code, written in Rust: the
//!   crate in that folder, built for `wasm32-wasip2` and copied to
//!   `server.wasm` next to its `plugin.json`.
//! - `cargo keygen`: the key pair releases are signed with, made once.
//! - `cargo release <version> [--dry-run]`: a release of the public repo
//!   (see `release.rs`).
//! - `cargo feed <exe>`: the signed update feed for a built `lsk.exe`, made
//!   by the release workflow (see `feed.rs`).

mod feed;
mod release;

use std::hash::{DefaultHasher, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The crates built with trunk, with where their output goes (as in their
/// `Trunk.toml`): the overlay, the control panel, and the Rain, Alerts and
/// Chat display plugins' overlay pages.
const WEB: [(&str, &str); 5] = [
    ("overlay", "crates/overlay/dist"),
    ("panel", "crates/panel/dist"),
    ("rain", "crates/server/plugins/rain/overlay"),
    ("alert", "crates/server/plugins/alert/overlay"),
    ("chat", "crates/server/plugins/chat/overlay"),
];

/// The built-in plugins' server code: crates built for `wasm32-wasip2`,
/// with the plugin folder their `server.wasm` goes into.
const SERVERS: [(&str, &str); 1] = [("obs", "crates/server/plugins/obs")];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("dist") => dist(),
        Some("e2e") => e2e(&args[1..]),
        Some("streamdeck") => streamdeck(),
        Some("plugin") if args.len() == 2 => plugin(&PathBuf::from(&args[1])),
        Some("keygen") => keygen(),
        Some("release") => release::release(&workspace_root(), &args[1..]),
        Some("feed") => feed::feed(&args[1..]),
        _ => Err("usage: cargo dist | cargo e2e [--rebuild-pages] [test name filter] | cargo streamdeck | cargo plugin <folder> | cargo keygen | cargo release <version> [--dry-run] | cargo feed <exe>"
            .to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Builds the overlay, the control panel, the Rain, Alerts and Chat
/// display plugins' pages (into `crates/server/plugins/*/overlay/`) and the
/// built-in plugins' server code in release mode, then the server. The order is the point: a release server embeds whatever
/// `crates/overlay/dist/`, `crates/panel/dist/` and `crates/server/plugins/`
/// hold when it's compiled, which may otherwise be a `trunk watch` debug
/// build or nothing at all.
fn dist() -> Result<(), String> {
    let root = workspace_root();
    let exe = target_dir(&root).join("release").join(format!("lsk{}", std::env::consts::EXE_SUFFIX));
    check_replaceable(&exe)?;

    // Every page, every time: what's shipped is never a skipped build.
    trunk(&root, true, true)?;
    servers(&root)?;
    run(Command::new(cargo()).args(["build", "--release", "--package", "lsk"]).current_dir(&root))?;

    let bytes = std::fs::metadata(&exe).map_err(|error| format!("{}: {error}", exe.display()))?.len();
    println!();
    println!(
        "Built {} ({:.1} MB), overlay, control panel and built-in plugins included.",
        exe.display(),
        bytes as f64 / 1e6
    );
    println!("It keeps its settings in lsk.json in the folder it's started in; nothing else is needed.");
    Ok(())
}

/// Builds the panel and the overlay (debug) and the server, then runs the
/// end-to-end tests, which are ignored by a plain `cargo test`. Extra
/// arguments filter the tests by name; `--rebuild-pages` runs trunk for
/// every page even when nothing they're built from changed. The debug
/// server reads the panel and the overlay from their `dist/` folders at
/// runtime, so nothing is embedded.
fn e2e(args: &[String]) -> Result<(), String> {
    let rebuild_pages = args.iter().any(|arg| arg == "--rebuild-pages");
    let filter: Vec<&String> = args.iter().filter(|arg| *arg != "--rebuild-pages").collect();
    let root = workspace_root();
    let exe = target_dir(&root).join("debug").join(format!("lsk{}", std::env::consts::EXE_SUFFIX));
    check_replaceable(&exe)?;

    trunk(&root, false, rebuild_pages)?;
    servers(&root)?;
    run(Command::new(cargo()).args(["build", "--package", "lsk"]).current_dir(&root))?;
    // The plugins with server code that the tests install.
    for folder in [
        "examples/plugins/counter",
        "examples/plugins/bits-to-money",
        "examples/plugins/copypasta",
        "crates/e2e/plugins/host-check",
    ] {
        plugin(&root.join(folder))?;
    }
    run(Command::new(cargo())
        .args(["test", "--package", "e2e", "--"])
        .args(filter)
        .args(["--ignored", "--test-threads", "4"])
        .current_dir(&root))
}

/// Builds every crate in [`WEB`] with trunk. Each builds into a folder of
/// its own under `target/web/`, and only the files that changed are copied
/// to where it's served from: trunk rewrites every file each time, and a
/// release server embeds them, so new file times alone would make cargo
/// rebuild it (about 45 s) on every `cargo dist`. Trunk's output is the
/// same for the same sources, so nothing changed means nothing copied.
///
/// Unless `every` is set, trunk is skipped for a page whose sources didn't
/// change since its last build: trunk reruns wasm-bindgen every time,
/// about 3 s a page. The crate is built with cargo first, as trunk builds
/// it (so trunk's own cargo build is then a no-op), and the page counts as
/// unchanged when that `.wasm` and every file in the crate's folder
/// (`index.html`, `Trunk.toml`, `Cargo.toml`, styles, copied assets) hash
/// the same as at the last trunk build, kept in `target/web/<profile>/<crate>.stamp`.
/// The stamp is removed before trunk runs and written after it succeeds,
/// so a failed build is never taken for a finished one.
fn trunk(root: &Path, release: bool, every: bool) -> Result<(), String> {
    let profile = if release { "release" } else { "debug" };
    for (name, dist) in WEB {
        let dir = root.join("crates").join(name);
        let web = target_dir(root).join("web").join(profile);
        let staging = web.join(name);
        let stamp_file = web.join(format!("{name}.stamp"));

        let mut build = Command::new(cargo());
        build
            .args(["build", "--target", "wasm32-unknown-unknown", "--manifest-path"])
            .arg(dir.join("Cargo.toml"));
        if release {
            build.arg("--release");
        }
        run(build.current_dir(&dir))?;
        let wasm = target_dir(root).join("wasm32-unknown-unknown").join(profile).join(format!("{name}.wasm"));
        let stamp = page_stamp(&wasm, &dir).map_err(|error| format!("hashing {name}'s sources: {error}"))?;

        if !every && staging.is_dir() && std::fs::read_to_string(&stamp_file).ok() == Some(stamp.clone()) {
            println!("{name}: unchanged since its last trunk build, skipped");
        } else {
            if let Err(error) = std::fs::remove_file(&stamp_file)
                && error.kind() != ErrorKind::NotFound
            {
                return Err(format!("{}: {error}", stamp_file.display()));
            }
            let mut command = Command::new("trunk");
            command.arg("build");
            if release {
                command.arg("--release");
            }
            run(command.arg("--dist").arg(&staging).current_dir(&dir))?;
            std::fs::write(&stamp_file, &stamp)
                .map_err(|error| format!("{}: {error}", stamp_file.display()))?;
        }
        sync(&staging, &root.join(dist))
            .map_err(|error| format!("copying {name}'s build to {dist}: {error}"))?;
    }
    Ok(())
}

/// A hash of what a page is built from: its crate's `.wasm` as cargo built
/// it, and every file in the crate's folder (its own `dist/` aside), with
/// their paths. The same sources give the same hash.
fn page_stamp(wasm: &Path, dir: &Path) -> std::io::Result<String> {
    fn files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                files(&entry.path(), out)?;
            } else {
                out.push(entry.path());
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if entry.file_name() != "dist" {
                files(&entry.path(), &mut paths)?;
            }
        } else {
            paths.push(entry.path());
        }
    }
    paths.sort();
    let mut hasher = DefaultHasher::new();
    let wasm_bytes = std::fs::read(wasm)?;
    hasher.write_usize(wasm_bytes.len());
    hasher.write(&wasm_bytes);
    for path in paths {
        let relative = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(&path)?;
        hasher.write(relative.as_bytes());
        hasher.write_usize(bytes.len());
        hasher.write(&bytes);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

/// Builds every crate in [`SERVERS`] for `wasm32-wasip2` (release: it runs
/// under the app's time limit either way) and copies it to its plugin's
/// `server.wasm` if it changed, so a release server isn't rebuilt for
/// nothing (as with [`trunk`]).
fn servers(root: &Path) -> Result<(), String> {
    for (name, folder) in SERVERS {
        run(Command::new(cargo())
            .args(["build", "--release", "--target", "wasm32-wasip2", "--package", name])
            .current_dir(root))?;
        let built = target_dir(root).join("wasm32-wasip2").join("release").join(format!("{name}.wasm"));
        let bytes = std::fs::read(&built).map_err(|error| format!("{}: {error}", built.display()))?;
        let server = root.join(folder).join("server.wasm");
        if std::fs::read(&server).ok().as_ref() != Some(&bytes) {
            std::fs::write(&server, &bytes).map_err(|error| format!("{}: {error}", server.display()))?;
        }
    }
    Ok(())
}

/// Makes `to` hold what `from` holds, writing only the files whose bytes
/// differ (so the rest keep their times) and removing what `from` hasn't.
fn sync(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if target.is_file() {
                std::fs::remove_file(&target)?;
            }
            sync(&entry.path(), &target)?;
        } else {
            let bytes = std::fs::read(entry.path())?;
            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            }
            if std::fs::read(&target).ok().as_ref() != Some(&bytes) {
                std::fs::write(&target, &bytes)?;
            }
        }
    }
    for entry in std::fs::read_dir(to)? {
        let entry = entry?;
        if !from.join(entry.file_name()).exists() {
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

/// The Stream Deck plugin: its packages (from the lock file), its tests,
/// then the `.streamDeckPlugin` file, which installs with a double-click.
fn streamdeck() -> Result<(), String> {
    let dir = workspace_root().join("streamdeck");
    run(npm().arg("ci").current_dir(&dir))?;
    run(npm().arg("test").current_dir(&dir))?;
    run(npm().args(["run", "pack"]).current_dir(&dir))?;
    println!();
    println!(
        "Built {}: double-click it to install it in Stream Deck.",
        dir.join("dist").join("com.localstreamkit.streamdeck.streamDeckPlugin").display()
    );
    Ok(())
}

/// Builds the Rust crate in a plugin's folder for `wasm32-wasip2` (a
/// WebAssembly component) and copies it to the folder's `server.wasm`.
fn plugin(folder: &Path) -> Result<(), String> {
    let root = workspace_root();
    let folder = if folder.is_absolute() { folder.to_path_buf() } else { root.join(folder) };
    let manifest = folder.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&manifest).map_err(|error| format!("{}: {error}", manifest.display()))?;
    let name = text
        .lines()
        .find_map(|line| {
            line.trim().strip_prefix("name").map(|rest| rest.trim_start_matches([' ', '=']).trim_matches('"'))
        })
        .ok_or_else(|| format!("{} has no package name", manifest.display()))?
        .replace('-', "_");
    // Its own target folder: it isn't part of the workspace.
    let target = target_dir(&root).join("plugins");
    run(Command::new(cargo())
        .args(["build", "--release", "--target", "wasm32-wasip2", "--manifest-path"])
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(&folder))?;
    let built = target.join("wasm32-wasip2").join("release").join(format!("{name}.wasm"));
    let server = folder.join("server.wasm");
    std::fs::copy(&built, &server).map_err(|error| format!("{}: {error}", built.display()))?;
    println!("Built {}", server.display());
    Ok(())
}

/// npm, which is a script on Windows.
fn npm() -> Command {
    if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/c", "npm"]);
        command
    } else {
        Command::new("npm")
    }
}

/// Windows can't replace an exe while it runs, so the server build would fail
/// at the very end. Check before spending minutes on the overlay and panel.
fn check_replaceable(exe: &Path) -> Result<(), String> {
    match std::fs::OpenOptions::new().write(true).open(exe) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(format!("can't replace {} ({error}); stop the running server first", exe.display()))
        }
        _ => Ok(()),
    }
}

/// Runs a build step with its output passed through.
fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status().map_err(|error| match error.kind() {
        ErrorKind::NotFound => format!("{program} not found; is it installed and on PATH?"),
        _ => format!("couldn't run {program}: {error}"),
    })?;
    if status.success() { Ok(()) } else { Err(format!("{program} failed ({status})")) }
}

fn workspace_root() -> PathBuf {
    // `cargo run` sets CARGO_MANIFEST_DIR when it starts us. Read it then
    // rather than at compile time: cargo doesn't rebuild xtask when the repo
    // folder is moved, so the compiled-in path can point at the old one.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    // crates/xtask → crates → workspace root
    manifest_dir.ancestors().nth(2).expect("xtask lives in crates/xtask").to_path_buf()
}

/// The cargo that started this task, so both builds use the same toolchain.
fn cargo() -> PathBuf {
    std::env::var_os("CARGO").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("cargo"))
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(|dir| root.join(dir)).unwrap_or_else(|| root.join("target"))
}

/// Makes the key pair releases are signed with, once, and prints both
/// halves; nothing is written to disk. The private key goes into the
/// user's pass store and the `Release` environment's `LSK_RELEASE_KEY`
/// secret; the public key into `PUBLIC_KEY` in
/// `crates/protocol/src/update.rs`.
fn keygen() -> Result<(), String> {
    let (private, public) = key_pair()?;
    println!("Private key: in pass, and as the Release environment's LSK_RELEASE_KEY secret.");
    println!("Never commit it or paste it anywhere else.");
    println!("{private}");
    println!();
    println!("Public key: PUBLIC_KEY in crates/protocol/src/update.rs.");
    println!("{public}");
    Ok(())
}

/// A new ed25519 key pair: the private key as standard base64 of its
/// PKCS#8 (v2) document, the public key as standard base64 of its 32
/// bytes, the form the app checks updates with.
fn key_pair() -> Result<(String, String), String> {
    use base64::Engine;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    let base64 = base64::engine::general_purpose::STANDARD;
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .map_err(|_| "couldn't make a key".to_string())?;
    let pair =
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "couldn't read the new key".to_string())?;
    Ok((base64.encode(pkcs8.as_ref()), base64.encode(pair.public_key().as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};

    #[test]
    fn a_key_pair_signs_what_its_public_half_checks() {
        let base64 = base64::engine::general_purpose::STANDARD;
        let (private, public) = key_pair().unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(&base64.decode(&private).unwrap()).unwrap();
        let public = base64.decode(&public).unwrap();
        assert_eq!(public.len(), 32);
        let signature = pair.sign(b"latest.json");
        assert!(UnparsedPublicKey::new(&ED25519, &public).verify(b"latest.json", signature.as_ref()).is_ok());
        assert_ne!(key_pair().unwrap().0, private, "a new key every time");
    }
}
