//! `cargo release <version> [--dry-run]`: a version of the public repo.
//!
//! On a clean `main`, it sets the workspace version and commits it, builds
//! the public tree from that commit with git plumbing (without touching the
//! checkout), leaving out the internal docs, commits that tree on `release`
//! with the previous release commit as its only parent, tags it
//! `v<version>` and pushes the two to the public repo, where the release
//! workflow (`.github/workflows/release.yml`) builds the draft release.
//!
//! Before the tag, it runs the workflow's unit tests at the new version: a
//! test can pass at the version `main` had and fail at the new one, and a
//! pushed tag can't be moved, so that version would be lost.
//!
//! The internal docs are the project's own notes: every Markdown file in
//! the repo's root and in `docs/` except the ones in [`PUBLIC_ROOT`] and
//! [`PUBLIC_DOCS`]. A new doc is internal until it's added there.
//!
//! The Stream Deck plugin has a version of its own, raised by hand: a
//! release where the plugin changed but its version didn't is refused.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The Markdown files in the repo's root that are public.
const PUBLIC_ROOT: [&str; 2] = ["README.md", "LICENSE.md"];

/// The Markdown files in `docs/` that are public.
const PUBLIC_DOCS: [&str; 2] = ["docs/writing-a-plugin.md", "docs/control-api.md"];

/// The public repo's remote pushes `release` as its `main`, and nothing
/// else, so a plain `git push` to it can't publish `main`.
const PUSH_REFSPEC: &str = "refs/heads/release:refs/heads/main";

/// The branch with one commit per release: the public repo's `main`.
const RELEASE: &str = "refs/heads/release";

/// The Stream Deck plugin's manifest, whose `Version` is the plugin's own.
const STREAMDECK_MANIFEST: &str = "streamdeck/com.localstreamkit.streamdeck.sdPlugin/manifest.json";

/// The packages the release workflow runs the unit tests of.
const TESTED: [&str; 4] = ["protocol", "lsk", "panel", "xtask"];

/// The unit tests a release runs before it tags.
struct Tests<'a> {
    /// The packages, as `cargo test -p` takes them.
    packages: &'a [&'a str],
    /// What the tests read that isn't in the repo, from the workspace's
    /// root: the pages and server code `cargo e2e` builds.
    built: Vec<String>,
    /// Whether their output is shown: not in this file's own tests, where
    /// one fails on purpose.
    print: bool,
}

impl Tests<'_> {
    fn command(&self) -> Vec<&str> {
        let mut args = vec!["test", "--locked"];
        for package in self.packages {
            args.extend(["-p", package]);
        }
        args
    }
}

/// Runs `cargo release`: `args` are the version and `--dry-run`.
pub fn release(root: &Path, args: &[String]) -> Result<(), String> {
    let built = crate::WEB
        .iter()
        .map(|(_, dist)| dist.to_string())
        .chain(crate::SERVERS.iter().map(|(_, folder)| format!("{folder}/server.wasm")))
        .collect();
    release_with(root, args, &Tests { packages: &TESTED, built, print: true })
}

fn release_with(root: &Path, args: &[String], tests: &Tests) -> Result<(), String> {
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let rest: Vec<&String> = args.iter().filter(|arg| *arg != "--dry-run").collect();
    let [version] = rest[..] else {
        return Err("usage: cargo release <version> [--dry-run], the version as 0.2.0".to_string());
    };
    let missing: Vec<&str> =
        tests.built.iter().filter(|path| !root.join(path).exists()).map(String::as_str).collect();
    if !missing.is_empty() {
        return Err(format!(
            "the unit tests need what cargo e2e builds, and it isn't built ({}). Run cargo e2e (a \
             release needs it passing anyway), then cargo release again",
            missing.join(", ")
        ));
    }
    let git = Git::new(root);
    let plan = Plan::make(&git, version)?;
    if dry_run { plan.print_dry_run(&git, tests) } else { plan.apply(&git, root, tests) }
}

/// Whether `path` (as git writes it, with `/`) is one of the internal
/// docs, left out of the public tree.
pub fn is_internal(path: &str) -> bool {
    if !path.to_ascii_lowercase().ends_with(".md") {
        return false;
    }
    match path.rsplit_once('/') {
        None => !PUBLIC_ROOT.contains(&path),
        Some((folder, _)) => {
            (folder == "docs" || folder.starts_with("docs/")) && !PUBLIC_DOCS.contains(&path)
        }
    }
}

/// Everything a release is made of, checked and written as git objects,
/// before any branch, tag or file changes.
struct Plan {
    version: String,
    /// The remote that pushes `release` as the public `main`.
    remote: String,
    /// Where it pushes to.
    url: String,
    /// `main` now.
    head: String,
    /// The version `main` has now.
    old_version: String,
    /// The commit setting the new version on top of `head`, or `head`
    /// itself when it has that version already.
    version_commit: String,
    /// The last release commit, if there was one.
    parent: Option<String>,
    /// The version the last release had.
    parent_version: Option<String>,
    /// The commit on `release`.
    release_commit: String,
    public: Vec<String>,
    internal: Vec<String>,
}

impl Plan {
    /// Every check, then the version commit, the public tree and the
    /// release commit, as objects only: no ref or file changes yet.
    fn make(git: &Git, version: &str) -> Result<Plan, String> {
        let parsed = parse_version(version)?;
        let tag = format!("v{version}");

        match git.run(&["symbolic-ref", "-q", "--short", "HEAD"]) {
            Ok(branch) if branch == "main" => {}
            Ok(branch) => return Err(format!("cargo release runs on main; this checkout is on {branch}")),
            Err(_) => return Err("cargo release runs on main; this checkout isn't on a branch".to_string()),
        }
        let changes = git.run(&["status", "--porcelain", "--untracked-files=no"])?;
        if !changes.is_empty() {
            return Err(format!("main has changes that aren't committed; commit them first:\n{changes}"));
        }
        if git.verify(&format!("refs/tags/{tag}"))?.is_some() {
            return Err(format!("the tag {tag} exists already"));
        }

        let remote = release_remote(git)?;
        let url = git.run(&["remote", "get-url", "--push", &remote])?;
        println!("Fetching {remote} ({url})");
        git.passthrough(&["fetch", "--prune", &remote])?;
        let listed = git.run(&["ls-remote", &remote, "refs/heads/main", &format!("refs/tags/{tag}")])?;
        let remote_ref = |name: &str| {
            listed.lines().find_map(|line| {
                let (sha, line_ref) = line.split_once('\t')?;
                (line_ref == name).then(|| sha.to_string())
            })
        };
        if remote_ref(&format!("refs/tags/{tag}")).is_some() {
            return Err(format!("{remote} has the tag {tag} already"));
        }
        let parent = git.verify(&format!("{RELEASE}^{{commit}}"))?;
        match (remote_ref("refs/heads/main"), &parent) {
            (None, None) => {}
            (Some(theirs), Some(ours)) if theirs == *ours => {}
            (Some(theirs), Some(ours)) => {
                return Err(format!(
                    "{remote}'s main ({}) isn't the last release ({}): it changed there since. Look at \
                     it (git log {}..{}), bring the change into main as a patch, then make release \
                     its commit (git branch -f release {theirs}) and run cargo release again; \
                     otherwise this release would drop it",
                    short(&theirs),
                    short(ours),
                    short(ours),
                    short(&theirs),
                ));
            }
            (None, Some(ours)) => {
                return Err(format!(
                    "{remote} ({url}) has no main, but the last release is {} here: is it the public repo?",
                    short(ours)
                ));
            }
            (Some(theirs), None) => {
                return Err(format!(
                    "{remote}'s main has commits ({}) but there's no release branch here. If they're \
                     this project's releases: git branch release {theirs}",
                    short(&theirs)
                ));
            }
        }

        let head = git.run(&["rev-parse", "HEAD"])?;
        let cargo_toml = git.run_bytes(&["cat-file", "blob", "HEAD:Cargo.toml"], None, &[])?;
        let cargo_toml = String::from_utf8(cargo_toml).map_err(|_| "Cargo.toml isn't UTF-8".to_string())?;
        let old_version = workspace_version(&cargo_toml)?;
        let parent_version = match &parent {
            Some(parent) => {
                let toml =
                    git.run_bytes(&["cat-file", "blob", &format!("{parent}:Cargo.toml")], None, &[])?;
                Some(workspace_version(&String::from_utf8_lossy(&toml))?)
            }
            None => None,
        };
        if let Some(last) = &parent_version
            && semver::Version::parse(last).is_ok_and(|last| parsed <= last)
        {
            return Err(format!("{version} isn't newer than the last release, {last}"));
        }
        if semver::Version::parse(&old_version).is_ok_and(|old| parsed < old) {
            return Err(format!("{version} is older than the version main has already, {old_version}"));
        }

        let index = TempIndex::new(git)?;
        let env = [("GIT_INDEX_FILE", index.path.as_os_str())];
        git.run_with(&["read-tree", "HEAD"], None, &env)?;
        let version_commit = if old_version == version {
            head.clone()
        } else {
            let lock = git.run_bytes(&["cat-file", "blob", "HEAD:Cargo.lock"], None, &[])?;
            let lock = String::from_utf8(lock).map_err(|_| "Cargo.lock isn't UTF-8".to_string())?;
            let files = [
                ("Cargo.toml", set_workspace_version(&cargo_toml, version)?),
                ("Cargo.lock", set_lock_versions(&lock, &old_version, version)?),
            ];
            for (path, text) in files {
                let blob = git.run_with(&["hash-object", "-w", "--stdin"], Some(text.as_bytes()), &[])?;
                git.run_with(&["update-index", "--cacheinfo", &format!("100644,{blob},{path}")], None, &env)?;
            }
            let tree = git.run_with(&["write-tree"], None, &env)?;
            git.run(&["commit-tree", &tree, "-p", &head, "-m", &format!("Version {version}")])?
        };
        if let Some(parent) = &parent {
            let changed = git.run(&["diff", "--name-only", parent, &version_commit, "--", "streamdeck/"])?;
            let changed: Vec<&str> = changed.lines().filter(|path| in_streamdeck_plugin(path)).collect();
            let plugin_version = |commit: &str| {
                let manifest = git.run_bytes(
                    &["cat-file", "blob", &format!("{commit}:{STREAMDECK_MANIFEST}")],
                    None,
                    &[],
                );
                let manifest: serde_json::Value = serde_json::from_slice(&manifest.ok()?).ok()?;
                manifest["Version"].as_str().map(str::to_string)
            };
            if !changed.is_empty()
                && let Some(problem) = unbumped_plugin(
                    &changed,
                    plugin_version(parent).as_deref(),
                    plugin_version(&version_commit).as_deref(),
                )
            {
                return Err(problem);
            }
        }

        let mut public = Vec::new();
        let mut internal = Vec::new();
        let listing = git.run_bytes(&["ls-tree", "-r", "-z", &version_commit], None, &[])?;
        let mut blobs = Vec::new();
        for entry in listing.split(|byte| *byte == 0).filter(|entry| !entry.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let (meta, path) =
                entry.split_once('\t').ok_or_else(|| format!("git ls-tree wrote {entry:?}"))?;
            let mut meta = meta.split(' ');
            let (kind, sha) = (meta.nth(1).unwrap_or_default(), meta.next().unwrap_or_default());
            if is_internal(path) {
                internal.push(path.to_string());
            } else {
                public.push(path.to_string());
                if kind == "blob" {
                    blobs.push((sha.to_string(), path.to_string()));
                }
            }
        }
        let found = names_found(&git.blobs(&blobs)?, &internal);
        if !found.is_empty() {
            return Err(format!(
                "public files name internal ones, which aren't in the public repo; reword them:\n{}",
                found.join("\n")
            ));
        }
        if !internal.is_empty() {
            let mut remove = vec!["update-index", "--force-remove", "--"];
            remove.extend(internal.iter().map(String::as_str));
            git.run_with(&remove, None, &env)?;
        }
        let tree = git.run_with(&["write-tree"], None, &env)?;
        let message = format!("Local Stream Kit {version}");
        let mut commit_tree = vec!["commit-tree", tree.as_str(), "-m", message.as_str()];
        if let Some(parent) = &parent {
            commit_tree.extend(["-p", parent.as_str()]);
        }
        let release_commit = git.run(&commit_tree)?;

        Ok(Plan {
            version: version.to_string(),
            remote,
            url,
            head,
            old_version,
            version_commit,
            parent,
            parent_version,
            release_commit,
            public,
            internal,
        })
    }

    fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    fn version_changes(&self) -> bool {
        self.version_commit != self.head
    }

    /// Lists the public files and what's left out, and what a real run
    /// would do.
    fn print_dry_run(&self, git: &Git, tests: &Tests) -> Result<(), String> {
        println!();
        println!("Public files ({}):", self.public.len());
        for path in &self.public {
            println!("  {path}");
        }
        println!();
        println!("Left out ({}):", self.internal.len());
        for path in &self.internal {
            println!("  {path}");
        }
        println!();
        println!("A dry run: nothing was changed or pushed. cargo release {} would:", self.version);
        if self.version_changes() {
            println!(
                "- commit \"Version {}\" on main ({} in Cargo.toml and Cargo.lock, was {})",
                self.version, self.version, self.old_version
            );
        } else {
            println!("- leave main as it is: it has version {} already", self.version);
        }
        println!("- run the unit tests at {} (cargo {})", self.version, tests.command().join(" "));
        let changed = match (&self.parent, &self.parent_version) {
            (Some(parent), Some(last)) => {
                let files = git.run(&["diff", "--name-only", parent, &self.release_commit])?;
                format!("on top of {last}'s, {} files changed since", files.lines().count())
            }
            _ => "the first one, with no parent".to_string(),
        };
        println!("- commit \"Local Stream Kit {}\" on release ({changed})", self.version);
        println!("- tag it {}", self.tag());
        println!("- push release as {}'s main, and the tag, to {}", self.remote, self.url);
        Ok(())
    }

    /// Moves `main`, runs the unit tests, then moves `release` and the tag
    /// and pushes.
    fn apply(&self, git: &Git, root: &Path, tests: &Tests) -> Result<(), String> {
        let tag = self.tag();
        if self.version_changes() {
            git.run(&["merge", "--ff-only", "-q", &self.version_commit])?;
            println!("Committed \"Version {}\" on main", self.version);
            // Cargo.lock was edited as text: make sure cargo agrees with it
            // before anything is tagged.
            let lock_check = Command::new(crate::cargo())
                .args(["metadata", "--format-version", "1", "--locked", "--offline", "--manifest-path"])
                .arg(root.join("Cargo.toml"))
                .stdout(Stdio::null())
                .status()
                .map_err(|error| format!("couldn't run cargo: {error}"))?;
            if !lock_check.success() {
                return Err(format!(
                    "Cargo.lock doesn't match Cargo.toml after the version change. main has the \
                     version commit, nothing else was done: fix Cargo.lock (cargo update --workspace), \
                     commit it and run cargo release {} again",
                    self.version
                ));
            }
        }
        println!("Running the unit tests at {}", self.version);
        let mut command = Command::new(crate::cargo());
        command.args(tests.command()).arg("--quiet").current_dir(root);
        if !tests.print {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let tested = command.status().map_err(|error| format!("couldn't run cargo: {error}"))?;
        if !tested.success() {
            let (kept, again) = if self.version_changes() {
                ("; main has the version commit", " (it makes no second one)")
            } else {
                ("", "")
            };
            return Err(format!(
                "the unit tests fail at {} (above), so nothing was tagged or pushed{kept}. Fix them, \
                 commit and run cargo release {} again{again}",
                self.version, self.version
            ));
        }
        let zero = "0".repeat(self.release_commit.len());
        let expected = self.parent.as_deref().unwrap_or(&zero);
        let message = format!("cargo release {}", self.version);
        git.run(&["update-ref", "-m", &message, RELEASE, &self.release_commit, expected])?;
        let title = format!("Local Stream Kit {}", self.version);
        git.run(&["tag", "-a", &tag, "-m", &title, &self.release_commit])?;
        println!("Committed \"{title}\" on release ({}) and tagged it {tag}", short(&self.release_commit));

        println!("Pushing release as main, and {tag}, to {} ({})", self.remote, self.url);
        let tag_ref = format!("refs/tags/{tag}:refs/tags/{tag}");
        if let Err(error) = git.passthrough(&["push", "--atomic", &self.remote, PUSH_REFSPEC, &tag_ref]) {
            let undo = match &self.parent {
                Some(parent) => format!("git update-ref {RELEASE} {parent}"),
                None => format!("git update-ref -d {RELEASE}"),
            };
            return Err(format!(
                "{error}. Nothing was pushed (the push is atomic). To push again:\n  git push --atomic {} \
                 {PUSH_REFSPEC} {tag_ref}\nor to undo the release here (main keeps its version commit):\n  \
                 git tag -d {tag}\n  {undo}",
                self.remote
            ));
        }
        let releases = protocol::update::releases();
        println!();
        println!("Pushed {tag}. The release workflow builds it into a draft release ({releases}/actions);");
        println!("then write the notes in the draft ({releases}/releases) and Publish.");
        Ok(())
    }
}

/// A version as `cargo release` takes it: semver, no `v`, no build
/// metadata (which semver ignores when comparing, so two releases could
/// count as the same version).
fn parse_version(version: &str) -> Result<semver::Version, String> {
    if let Some(without) = version.strip_prefix('v') {
        return Err(format!("the version without the v: {without}"));
    }
    let parsed = semver::Version::parse(version)
        .map_err(|error| format!("{version} isn't a version like 0.2.0 ({error})"))?;
    if !parsed.build.is_empty() {
        return Err(format!(
            "{version} has build metadata (+{}), which versions are compared without",
            parsed.build
        ));
    }
    Ok(parsed)
}

/// Whether a change to `path` changes the Stream Deck plugin people
/// install (its code, files, packages, or how it's built and packed), so
/// it needs a new version: everything under `streamdeck/` but its README,
/// its tests and the scripts that aren't part of it.
fn in_streamdeck_plugin(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("streamdeck/") else { return false };
    let not_in_it = rest == "README.md"
        || rest == ".gitignore"
        || rest == "tsconfig.test.json"
        || rest.ends_with(".test.ts")
        || (rest.starts_with("scripts/") && rest != "scripts/pack.mjs");
    !not_in_it
}

/// Why a release can't go out: the Stream Deck plugin's files `changed`
/// since the last release, whose plugin was version `last`, but its version
/// (`now`) isn't newer. None when there was no plugin before.
fn unbumped_plugin(changed: &[&str], last: Option<&str>, now: Option<&str>) -> Option<String> {
    let last = last?;
    if now.is_some_and(|now| protocol::streamdeck::is_older(last, now)) {
        return None;
    }
    let mut files = changed.iter().take(5).copied().collect::<Vec<_>>().join(", ");
    if changed.len() > 5 {
        files.push_str(", …");
    }
    Some(format!(
        "the Stream Deck plugin changed since the last release ({files}), but its version is {} (it was \
         {last}): raise Version in {STREAMDECK_MANIFEST}, so Status offers the update",
        now.unwrap_or("missing")
    ))
}

/// The remote whose only push refspec is [`PUSH_REFSPEC`].
fn release_remote(git: &Git) -> Result<String, String> {
    let configured = git.run(&["config", "--get-regexp", r"^remote\..*\.push$"]).unwrap_or_default();
    let mut pushes: Vec<(String, String)> = Vec::new();
    for line in configured.lines() {
        if let Some((key, value)) = line.split_once(' ')
            && let Some(name) = key.strip_prefix("remote.").and_then(|key| key.strip_suffix(".push"))
        {
            pushes.push((name.to_string(), value.to_string()));
        }
    }
    let remotes: BTreeSet<&str> =
        pushes.iter().filter(|(_, value)| value == PUSH_REFSPEC).map(|(name, _)| name.as_str()).collect();
    match remotes.into_iter().collect::<Vec<_>>()[..] {
        [] => {
            let url = format!("git@github.com:{}.git", protocol::update::REPO);
            Err(format!(
                "no remote pushes release as the public repo's main. Add it (if it's there already, \
                 only the second line):\n  git remote add public {url}\n  git config remote.public.push \
                 {PUSH_REFSPEC}"
            ))
        }
        [remote] => {
            let others: Vec<&str> = pushes
                .iter()
                .filter(|(name, value)| name == remote && value != PUSH_REFSPEC)
                .map(|(_, value)| value.as_str())
                .collect();
            if others.is_empty() {
                Ok(remote.to_string())
            } else {
                Err(format!(
                    "the remote {remote} also pushes {}: it should only push {PUSH_REFSPEC}",
                    others.join(", ")
                ))
            }
        }
        ref several => Err(format!("several remotes push {PUSH_REFSPEC}: {}", several.join(", "))),
    }
}

/// The `version` in `Cargo.toml`'s `[workspace.package]`.
fn workspace_version(cargo_toml: &str) -> Result<String, String> {
    let (_, line) = workspace_version_line(cargo_toml)?;
    let value = line.split_once('=').map(|(_, value)| value.trim()).unwrap_or_default();
    Ok(value.trim_matches('"').to_string())
}

/// `Cargo.toml` with `[workspace.package]`'s version set to `version`.
fn set_workspace_version(cargo_toml: &str, version: &str) -> Result<String, String> {
    let (number, _) = workspace_version_line(cargo_toml)?;
    let mut out = String::with_capacity(cargo_toml.len());
    for (index, line) in cargo_toml.split_inclusive('\n').enumerate() {
        if index == number {
            out.push_str(&format!("version = \"{version}\"\n"));
        } else {
            out.push_str(line);
        }
    }
    Ok(out)
}

/// The line number and text of `version = "…"` in `[workspace.package]`.
fn workspace_version_line(cargo_toml: &str) -> Result<(usize, &str), String> {
    let mut section = "";
    for (number, line) in cargo_toml.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed;
        } else if section == "[workspace.package]"
            && trimmed
                .split_once('=')
                .is_some_and(|(key, value)| key.trim() == "version" && value.contains('"'))
        {
            return Ok((number, trimmed));
        }
    }
    Err("Cargo.toml has no version in [workspace.package]".to_string())
}

/// `Cargo.lock` with the workspace's own packages (no `source`, as path
/// packages are written) at version `old` set to `new`.
fn set_lock_versions(lock: &str, old: &str, new: &str) -> Result<String, String> {
    let mut blocks: Vec<String> = Vec::new();
    for line in lock.split_inclusive('\n') {
        if line.trim_end() == "[[package]]" || blocks.is_empty() {
            blocks.push(String::new());
        }
        blocks.last_mut().expect("a block was pushed").push_str(line);
    }
    let old_line = format!("version = \"{old}\"");
    let mut changed = 0;
    for block in &mut blocks {
        let is_package = block.starts_with("[[package]]");
        let is_local = !block.lines().any(|line| line.starts_with("source = "));
        if is_package && is_local && block.lines().any(|line| line == old_line) {
            *block = block
                .split_inclusive('\n')
                .map(|line| {
                    if line.trim_end() == old_line {
                        format!("version = \"{new}\"\n")
                    } else {
                        line.to_string()
                    }
                })
                .collect();
            changed += 1;
        }
    }
    if changed == 0 {
        return Err(format!("Cargo.lock has none of the workspace's packages at {old}"));
    }
    Ok(blocks.concat())
}

/// Where public files name internal ones, as `path:line: names
/// internal/path`. `files` are the public files' paths and bytes. A name
/// counts when it's a whole file name, in any case (`Notes.md`,
/// `../notes.md`, `docs/notes.md#a-part`), not part of a longer one
/// (`my-notes.md`). Binary files are skipped.
fn names_found(files: &[(String, Vec<u8>)], internal: &[String]) -> Vec<String> {
    let names: Vec<(String, &String)> = internal
        .iter()
        .map(|path| (path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase(), path))
        .collect();
    let part_of_a_name = |byte: u8| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte);
    let mut found = Vec::new();
    for (path, bytes) in files {
        if bytes[..bytes.len().min(8000)].contains(&0) {
            continue;
        }
        let text = bytes.to_ascii_lowercase();
        for (name, internal_path) in &names {
            let name = name.as_bytes();
            let mut at = 0;
            while let Some(offset) = find(&text[at..], name) {
                let start = at + offset;
                let end = start + name.len();
                let before = start.checked_sub(1).map(|index| text[index]);
                let after = text.get(end).copied();
                if !before.is_some_and(part_of_a_name)
                    && !after.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    let line = text[..start].iter().filter(|byte| **byte == b'\n').count() + 1;
                    found.push(format!("  {path}:{line}: names {internal_path}"));
                }
                at = end;
            }
        }
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// git in the repo at `dir`.
struct Git {
    dir: PathBuf,
}

impl Git {
    fn new(dir: &Path) -> Git {
        Git { dir: dir.to_path_buf() }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.dir).args(args);
        command
    }

    /// Runs git and returns what it wrote, trimmed.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.run_with(args, None, &[])
    }

    fn run_with(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &OsStr)],
    ) -> Result<String, String> {
        let bytes = self.run_bytes(args, input, env)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_string())
    }

    /// Runs git with `input` on its stdin and `env` set, and returns its
    /// output; when it fails, the error has what it said.
    fn run_bytes(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        env: &[(&str, &OsStr)],
    ) -> Result<Vec<u8>, String> {
        let mut command = self.command(args);
        command.envs(env.iter().copied()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
        let mut child = command.spawn().map_err(|error| format!("couldn't run git: {error}"))?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            stdin.write_all(input).map_err(|error| format!("writing to git {}: {error}", args[0]))?;
        }
        let output = child.wait_with_output().map_err(|error| format!("git {}: {error}", args[0]))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let said = String::from_utf8_lossy(&output.stderr);
            Err(format!("git {} failed: {}", args.join(" "), said.trim()))
        }
    }

    /// Runs git with its output shown as it goes (a fetch, a push).
    fn passthrough(&self, args: &[&str]) -> Result<(), String> {
        let status = self
            .command(args)
            .stdin(Stdio::null())
            .status()
            .map_err(|error| format!("couldn't run git: {error}"))?;
        if status.success() { Ok(()) } else { Err(format!("git {} failed", args[0])) }
    }

    /// The object `name` names, if there is one.
    fn verify(&self, name: &str) -> Result<Option<String>, String> {
        let output = self
            .command(&["rev-parse", "-q", "--verify", name])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("couldn't run git: {error}"))?;
        Ok(output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    /// The bytes of the blobs `(sha, path)`, with their paths, read in one
    /// `git cat-file --batch`.
    fn blobs(&self, blobs: &[(String, String)]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut child = self
            .command(&["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("couldn't run git: {error}"))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let shas: String = blobs.iter().map(|(sha, _)| format!("{sha}\n")).collect();
        let writer = std::thread::spawn(move || stdin.write_all(shas.as_bytes()));
        let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut out = Vec::with_capacity(blobs.len());
        for (sha, path) in blobs {
            let mut header = String::new();
            reader.read_line(&mut header).map_err(|error| format!("reading {path}: {error}"))?;
            let size: usize = header
                .split(' ')
                .nth(2)
                .and_then(|size| size.trim().parse().ok())
                .ok_or_else(|| format!("git cat-file has no {sha} ({path}): {}", header.trim()))?;
            let mut bytes = vec![0; size + 1];
            reader.read_exact(&mut bytes).map_err(|error| format!("reading {path}: {error}"))?;
            bytes.pop();
            out.push((path.clone(), bytes));
        }
        writer.join().expect("the writer doesn't panic").map_err(|error| format!("git cat-file: {error}"))?;
        child.wait().map_err(|error| format!("git cat-file: {error}"))?;
        Ok(out)
    }
}

/// An index file of its own in the repo's git folder, so the public tree
/// is built without touching the checkout's index; removed when dropped.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn new(git: &Git) -> Result<TempIndex, String> {
        let dir = git.run(&["rev-parse", "--absolute-git-dir"])?;
        let path = PathBuf::from(dir).join("lsk-release-index");
        let _ = std::fs::remove_file(&path);
        Ok(TempIndex { path })
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_docs() {
        for path in ["NOTES.md", "docs/design.md", "docs/review-2026-01-01.md", "docs/old/notes.md"] {
            assert!(is_internal(path), "{path}");
        }
        for path in [
            "README.md",
            "LICENSE.md",
            "docs/writing-a-plugin.md",
            "docs/control-api.md",
            "docs/images/card-editor.png",
            "streamdeck/README.md",
            "examples/plugins/README.md",
            "crates/server/plugins/obs/notes.md",
            "Cargo.toml",
        ] {
            assert!(!is_internal(path), "{path}");
        }
    }

    /// The release workflow starts each draft's notes from
    /// `.github/release-notes.md`: whoever downloads the exe gets the
    /// licence and its `Required Notice:` line there. They name the Stream
    /// Deck plugin's file, which the workflow attaches.
    #[test]
    fn the_release_notes_have_the_licence() {
        let notes = include_str!("../../../.github/release-notes.md");
        assert!(notes.contains(protocol::streamdeck::PLUGIN_FILE));
        let workflow = include_str!("../../../.github/workflows/release.yml");
        assert!(workflow.contains(&format!("streamdeck/dist/{}", protocol::streamdeck::PLUGIN_FILE)));
        assert!(notes.lines().any(|line| line == protocol::update::REQUIRED_NOTICE));
        assert!(notes.contains(&format!(
            "[{}]({})",
            protocol::update::LICENCE,
            protocol::update::licence_url()
        )));
    }

    /// A release runs the unit tests the release workflow runs, so a tag
    /// never meets tests it hasn't passed.
    #[test]
    fn the_workflows_unit_tests() {
        let workflow = include_str!("../../../.github/workflows/release.yml");
        let tests = Tests { packages: &TESTED, built: Vec::new(), print: false };
        assert!(workflow.contains(&format!("cargo {}\n", tests.command().join(" "))));
    }

    #[test]
    fn the_stream_deck_plugins_own_files() {
        for path in [
            "streamdeck/src/app.ts",
            "streamdeck/package-lock.json",
            "streamdeck/rollup.config.mjs",
            "streamdeck/scripts/pack.mjs",
            "streamdeck/com.localstreamkit.streamdeck.sdPlugin/imgs/plugin.png",
        ] {
            assert!(in_streamdeck_plugin(path), "{path}");
        }
        for path in [
            "streamdeck/README.md",
            "streamdeck/src/view.test.ts",
            "streamdeck/tsconfig.test.json",
            "streamdeck/scripts/fake-stream-deck.mjs",
            "crates/server/src/streamdeck.rs",
        ] {
            assert!(!in_streamdeck_plugin(path), "{path}");
        }
        let changed = ["streamdeck/src/app.ts"];
        assert!(unbumped_plugin(&changed, Some("0.2.0.0"), Some("0.2.1.0")).is_none());
        let problem = unbumped_plugin(&changed, Some("0.2.0.0"), Some("0.2.0.0")).unwrap();
        assert!(problem.contains("streamdeck/src/app.ts") && problem.contains("raise Version"), "{problem}");
        assert!(unbumped_plugin(&changed, Some("0.2.0.0"), None).is_some());
        assert!(unbumped_plugin(&changed, None, Some("0.1.0.0")).is_none(), "no plugin before");
    }

    #[test]
    fn versions() {
        assert!(parse_version("0.2.0").is_ok());
        assert!(parse_version("1.0.0-rc.1").is_ok());
        assert!(parse_version("v0.2.0").unwrap_err().contains("without the v"));
        assert!(parse_version("0.2").is_err());
        assert!(parse_version("0.2.0+build").unwrap_err().contains("build metadata"));
    }

    #[test]
    fn the_version_is_set_in_cargo_toml_and_cargo_lock() {
        let toml = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \
                    \"2024\"\n\n[workspace.dependencies]\nserde = { version = \"1\" }\n";
        assert_eq!(workspace_version(toml).unwrap(), "0.1.0");
        let set = set_workspace_version(toml, "0.2.0").unwrap();
        assert_eq!(set, toml.replace("version = \"0.1.0\"", "version = \"0.2.0\""));
        assert!(workspace_version("[package]\nversion = \"1.0.0\"\n").is_err());

        let lock = "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\ndependencies = [\n \
                    \"serde\",\n]\n\n[[package]]\nname = \"serde\"\nversion = \"0.1.0\"\nsource = \
                    \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \
                    \"b\"\nversion = \"0.1.0\"\n";
        let set = set_lock_versions(lock, "0.1.0", "0.2.0").unwrap();
        assert_eq!(set.matches("version = \"0.2.0\"").count(), 2, "a and b, not serde from crates.io");
        assert!(set.contains("name = \"serde\"\nversion = \"0.1.0\"\nsource"));
        assert!(set.starts_with("version = 4\n"));
        assert!(set_lock_versions(lock, "9.9.9", "10.0.0").is_err());
    }

    #[test]
    fn names_of_internal_files_are_found() {
        let internal = vec!["NOTES.md".to_string(), "docs/design.md".to_string()];
        let file = |path: &str, text: &str| (path.to_string(), text.as_bytes().to_vec());
        let files = [
            file("README.md", "See [the notes](NOTES.md).\nAnd docs/Design.md#a-part\n"),
            file("src/lib.rs", "// my-notes.md and design.mdx are other files\n// ../design.md\n"),
            file("logo.png", "\0design.md"),
        ];
        assert_eq!(
            names_found(&files, &internal),
            [
                "  README.md:1: names NOTES.md",
                "  README.md:2: names docs/design.md",
                "  src/lib.rs:2: names docs/design.md"
            ]
        );
    }

    /// A repo in a fresh temporary folder, removed when dropped.
    struct Repo {
        dir: PathBuf,
    }

    impl Repo {
        fn new(name: &str) -> Repo {
            let dir = std::env::temp_dir().join(format!("lsk-release-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Repo { dir }
        }

        fn git(&self, folder: &str, args: &[&str]) -> String {
            Git::new(&self.dir.join(folder)).run(args).unwrap()
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.dir.join("work").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// A release that tests crate `a`, the stand-in workspace's.
        fn release(&self, args: &[&str]) -> Result<(), String> {
            self.release_needing(args, &[])
        }

        fn release_needing(&self, args: &[&str], built: &[&str]) -> Result<(), String> {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            let built = built.iter().map(|path| path.to_string()).collect();
            release_with(&self.dir.join("work"), &args, &Tests { packages: &["a"], built, print: false })
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Two releases pushed to a bare repo standing in for the public one,
    /// and the checks that stop one.
    #[test]
    fn releases_go_to_the_public_repo() {
        let repo = Repo::new("two");
        std::fs::create_dir_all(repo.dir.join("work")).unwrap();
        repo.git("work", &["init", "-q", "-b", "main"]);
        repo.git("work", &["config", "user.name", "Test"]);
        repo.git("work", &["config", "user.email", "test@example.com"]);
        repo.git("work", &["config", "core.autocrlf", "false"]);
        // Where the tests' cargo builds.
        std::fs::create_dir_all(repo.dir.join("work/.git/info")).unwrap();
        std::fs::write(repo.dir.join("work/.git/info/exclude"), "/target/\n").unwrap();
        repo.git(".", &["init", "-q", "--bare", "-b", "main", "public.git"]);
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\"]\nresolver = \"3\"\n\n[workspace.package]\nversion = \
             \"0.1.0\"\nedition = \"2024\"\n",
        );
        repo.write(
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        repo.write("crates/a/src/lib.rs", "//! A crate.\n");
        repo.write(
            "Cargo.lock",
            "# This file is automatically @generated by Cargo.\n# It is not intended for manual \
             editing.\nversion = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        repo.write("README.md", "# A\n");
        repo.write(STREAMDECK_MANIFEST, "{ \"Version\": \"0.1.0.0\" }\n");
        repo.write("streamdeck/src/plugin.ts", "// The plugin.\n");
        repo.write("NOTES.md", "internal\n");
        repo.write("docs/design.md", "internal\n");
        repo.write("docs/writing-a-plugin.md", "public\n");
        repo.git("work", &["add", "."]);
        repo.git("work", &["commit", "-q", "-m", "Start"]);

        let error = repo.release(&["0.1.0"]).unwrap_err();
        assert!(error.contains("git remote add public git@github.com:"), "{error}");
        let public = repo.dir.join("public.git");
        repo.git("work", &["remote", "add", "public", public.to_str().unwrap()]);
        repo.git("work", &["config", "remote.public.push", PUSH_REFSPEC]);

        assert!(repo.release(&["v0.1.0"]).unwrap_err().contains("without the v"));
        let error = repo.release_needing(&["0.1.0", "--dry-run"], &["crates/a/dist"]).unwrap_err();
        assert!(error.contains("isn't built (crates/a/dist). Run cargo e2e"), "{error}");
        repo.release(&["0.1.0", "--dry-run"]).unwrap();
        assert!(Git::new(&public).verify("refs/heads/main").unwrap().is_none(), "a dry run pushes nothing");
        let work = Git::new(&repo.dir.join("work"));
        assert!(work.verify(RELEASE).unwrap().is_none(), "a dry run makes no release");
        assert!(work.verify("refs/tags/v0.1.0").unwrap().is_none());

        // The first release: main has its version already.
        repo.release(&["0.1.0"]).unwrap();
        let first = repo.git("public.git", &["rev-parse", "main"]);
        assert_eq!(repo.git("public.git", &["rev-parse", "v0.1.0^{commit}"]), first);
        assert_eq!(repo.git("public.git", &["cat-file", "-t", "v0.1.0"]), "tag", "an annotated tag");
        assert_eq!(repo.git("public.git", &["log", "--format=%s %P", "main"]), "Local Stream Kit 0.1.0");
        assert_eq!(
            repo.git("public.git", &["ls-tree", "-r", "--name-only", "main"]),
            format!(
                "Cargo.lock\nCargo.toml\nREADME.md\ncrates/a/Cargo.toml\ncrates/a/src/lib.rs\ndocs/writing-a-plugin.md\n\
                 {STREAMDECK_MANIFEST}\nstreamdeck/src/plugin.ts"
            )
        );
        assert_eq!(repo.git("work", &["log", "--format=%s", "main"]), "Start", "no version commit");
        assert_eq!(repo.git("work", &["status", "--porcelain"]), "");

        assert!(repo.release(&["0.1.0"]).unwrap_err().contains("exists already"));
        assert!(repo.release(&["0.0.9"]).unwrap_err().contains("isn't newer"));
        repo.write("README.md", "# A\n\nMore.\n");
        assert!(repo.release(&["0.2.0"]).unwrap_err().contains("aren't committed"));
        repo.git("work", &["commit", "-q", "-am", "More"]);
        repo.write("crates/a/src/lib.rs", "//! A crate. See NOTES.md.\n");
        repo.git("work", &["commit", "-q", "-am", "A pointer"]);
        let error = repo.release(&["0.2.0"]).unwrap_err();
        assert!(error.contains("crates/a/src/lib.rs:1: names NOTES.md"), "{error}");
        repo.write("crates/a/src/lib.rs", "//! A crate.\n");
        repo.git("work", &["commit", "-q", "-am", "No pointer"]);
        // The plugin changed: its version must go up.
        repo.write("streamdeck/src/plugin.ts", "// The plugin, changed.\n");
        repo.git("work", &["commit", "-q", "-am", "Plugin"]);
        let error = repo.release(&["0.2.0"]).unwrap_err();
        assert!(error.contains("its version is 0.1.0.0"), "{error}");
        repo.write(STREAMDECK_MANIFEST, "{ \"Version\": \"0.1.1.0\" }\n");
        repo.git("work", &["commit", "-q", "-am", "Plugin 0.1.1.0"]);

        // A test that passes at 0.1.0 and fails at 0.2.0 stops the release
        // before anything is tagged, main keeping the version commit.
        let lib =
            |assertion: &str| format!("//! A crate.\n\n#[test]\nfn version() {{\n    {assertion};\n}}\n");
        repo.write("crates/a/src/lib.rs", &lib("assert_eq!(env!(\"CARGO_PKG_VERSION\"), \"0.1.0\")"));
        repo.git("work", &["commit", "-q", "-am", "A test"]);
        let error = repo.release(&["0.2.0"]).unwrap_err();
        assert!(error.contains("the unit tests fail at 0.2.0"), "{error}");
        assert_eq!(repo.git("work", &["log", "-1", "--format=%s", "main"]), "Version 0.2.0");
        assert_eq!(work.verify(RELEASE).unwrap(), Some(first.clone()), "release didn't move");
        assert!(work.verify("refs/tags/v0.2.0").unwrap().is_none(), "no tag");
        assert_eq!(repo.git("public.git", &["rev-parse", "main"]), first, "nothing pushed");
        repo.write("crates/a/src/lib.rs", &lib("assert!(!env!(\"CARGO_PKG_VERSION\").is_empty())"));
        repo.git("work", &["commit", "-q", "-am", "A test for any version"]);

        // The second: a version commit on main (the one made already), a
        // release on top of the first.
        repo.release(&["0.2.0"]).unwrap();
        assert_eq!(
            repo.git("work", &["log", "-2", "--format=%s", "main"]),
            "A test for any version\nVersion 0.2.0",
            "no second version commit"
        );
        assert_eq!(repo.git("work", &["status", "--porcelain"]), "", "the checkout has the version commit");
        let toml = std::fs::read_to_string(repo.dir.join("work/Cargo.toml")).unwrap();
        assert!(toml.contains("version = \"0.2.0\""));
        let lock = std::fs::read_to_string(repo.dir.join("work/Cargo.lock")).unwrap();
        assert!(lock.contains("name = \"a\"\nversion = \"0.2.0\""));
        assert_eq!(
            repo.git("public.git", &["log", "--format=%s %P", "main"]),
            format!("Local Stream Kit 0.2.0 {first}\nLocal Stream Kit 0.1.0"),
            "one commit per release, each on the one before"
        );
        assert_eq!(repo.git("public.git", &["show", "main:README.md"]), "# A\n\nMore.");
        assert!(repo.git("public.git", &["show", "v0.2.0:Cargo.toml"]).contains("version = \"0.2.0\""));
        assert_eq!(
            repo.git("work", &["rev-parse", "release"]),
            repo.git("public.git", &["rev-parse", "main"])
        );

        // A change made on the public repo stops the next release.
        repo.git(".", &["clone", "-q", "-b", "main", public.to_str().unwrap(), "elsewhere"]);
        repo.git("elsewhere", &["config", "user.name", "Test"]);
        repo.git("elsewhere", &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.dir.join("elsewhere/README.md"), "# A, changed there\n").unwrap();
        repo.git("elsewhere", &["commit", "-q", "-am", "Changed there"]);
        repo.git("elsewhere", &["push", "-q", "origin", "main"]);
        let error = repo.release(&["0.3.0"]).unwrap_err();
        assert!(error.contains("changed there since"), "{error}");
        assert_eq!(
            repo.git("work", &["log", "-1", "--format=%s", "main"]),
            "A test for any version",
            "nothing done"
        );

        repo.git("work", &["config", "--add", "remote.public.push", "refs/heads/main:refs/heads/main"]);
        assert!(repo.release(&["0.3.0"]).unwrap_err().contains("also pushes refs/heads/main"));
        repo.git("work", &["checkout", "-q", "-b", "other"]);
        assert!(repo.release(&["0.3.0"]).unwrap_err().contains("runs on main"));
    }
}
