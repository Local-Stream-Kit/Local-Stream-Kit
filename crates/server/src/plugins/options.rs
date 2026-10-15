//! Lists of choices plugins' server code fills in (`host.set-options`,
//! or `host.set-choices` with a label for each value): OBS's scenes, say.
//! Text settings whose `suggest_from` names one offer its choices on the
//! panel. Kept while the app runs, not saved: server code fills them in
//! again when it starts. See `docs/writing-a-plugin.md` → Settings.

use std::collections::BTreeMap;

use protocol::plugin::{Choice, MAX_OPTION_LENGTH, MAX_OPTION_LISTS, MAX_OPTIONS};
use tokio::sync::watch;

/// Choices by plugin and list name.
pub type Options = BTreeMap<String, BTreeMap<String, Vec<Choice>>>;

#[derive(Clone)]
pub struct PluginOptions {
    options: watch::Sender<Options>,
}

impl Default for PluginOptions {
    fn default() -> Self {
        PluginOptions { options: watch::Sender::new(Options::new()) }
    }
}

impl PluginOptions {
    pub fn subscribe(&self) -> watch::Receiver<Options> {
        self.options.subscribe()
    }

    /// Replaces one of `plugin`'s lists; an empty one takes it away. Only
    /// the first [`MAX_OPTIONS`] choices are kept, choices longer than
    /// [`MAX_OPTION_LENGTH`] are left out, and a new list past
    /// [`MAX_OPTION_LISTS`] is too; a warning says so. Nobody is told when
    /// nothing changed. An empty label is its value.
    pub fn set(&self, plugin: &str, list: &str, options: Vec<Choice>) {
        if list.is_empty() || list.len() > MAX_OPTION_LENGTH {
            tracing::warn!(plugin, "set-options: a list's name must be 1 to {MAX_OPTION_LENGTH} bytes");
            return;
        }
        let given = options.len();
        let fits = |text: &str| !text.is_empty() && text.len() <= MAX_OPTION_LENGTH;
        let options: Vec<Choice> = options
            .into_iter()
            .map(|choice| if choice.label.trim().is_empty() { Choice::plain(choice.value) } else { choice })
            .filter(|choice| fits(&choice.value) && fits(&choice.label))
            .take(MAX_OPTIONS)
            .collect();
        if options.len() < given {
            tracing::warn!(
                plugin,
                list,
                "set-options: {} of its choices were left out (empty, longer than {MAX_OPTION_LENGTH} bytes, or past {MAX_OPTIONS})",
                given - options.len()
            );
        }
        self.options.send_if_modified(|all| {
            let lists = all.entry(plugin.to_string()).or_default();
            let changed = if options.is_empty() {
                lists.remove(list).is_some()
            } else if !lists.contains_key(list) && lists.len() >= MAX_OPTION_LISTS {
                tracing::warn!(plugin, list, "set-options: it has {MAX_OPTION_LISTS} lists already");
                false
            } else {
                lists.get(list) != Some(&options) && {
                    lists.insert(list.to_string(), options);
                    true
                }
            };
            if lists.is_empty() {
                all.remove(plugin);
            }
            changed
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_set_replaced_and_taken_away() {
        let options = PluginOptions::default();
        let mut changes = options.subscribe();
        let plain = |names: &[&str]| names.iter().map(|name| Choice::plain(*name)).collect::<Vec<_>>();
        options.set("obs", "scenes", plain(&["Gaming", "Be right back"]));
        assert!(changes.has_changed().unwrap());
        assert_eq!(changes.borrow_and_update()["obs"]["scenes"], plain(&["Gaming", "Be right back"]));

        options.set("obs", "scenes", plain(&["Gaming", "Be right back"]));
        assert!(!changes.has_changed().unwrap(), "the same list again changes nothing");

        options.set("obs", "scenes", plain(&["Gaming", "", &"x".repeat(MAX_OPTION_LENGTH + 1)]));
        assert_eq!(changes.borrow_and_update()["obs"]["scenes"], plain(&["Gaming"]));

        // With labels (`set-choices`): an empty one is the value.
        let choice = |value: &str, label: &str| Choice { value: value.into(), label: label.into() };
        options.set("vtube", "emotes", vec![choice("wave", "Wave hello"), choice("nod", " ")]);
        assert_eq!(
            changes.borrow_and_update()["vtube"]["emotes"],
            [choice("wave", "Wave hello"), choice("nod", "nod")]
        );
        options.set("vtube", "emotes", Vec::new());

        options.set("obs", "scenes", Vec::new());
        assert!(changes.borrow_and_update().is_empty());

        options.set("obs", "", plain(&["x"]));
        assert!(!changes.has_changed().unwrap(), "a list needs a name");
    }
}
