//! Carrying a change made elsewhere (the control API, another panel, a hand
//! edit) into configs the panel holds: the one being edited, and the undo
//! and redo steps. A three-way merge of their JSON: what changed from the
//! server's old config to its new one is made in each, and whatever the
//! panel changed itself stays. Lists of things with an `id` (cards, nodes,
//! overlays…) are matched by id; where both changed the same thing, the
//! panel's own change wins (it's about to be saved).

use protocol::config::AppConfig;
use protocol::plugin::Catalog;
use serde_json::{Map, Value};

/// `config` with the change from `old` to `new` made in it.
pub fn rebase(config: &AppConfig, old: &AppConfig, new: &AppConfig, plugins: &Catalog) -> AppConfig {
    if old == new || config == new {
        return config.clone();
    }
    if config == old {
        return new.clone();
    }
    let json = |config: &AppConfig| serde_json::to_value(config).expect("configs serialise");
    let merged = merge(Some(&json(old)), Some(&json(config)), Some(&json(new)));
    match merged.map(serde_json::from_value::<AppConfig>) {
        Some(Ok(mut merged)) => {
            merged.attach_plugins(plugins);
            merged
        }
        // Can't happen with configs on both sides; keep the panel's.
        _ => config.clone(),
    }
}

/// Three-way merge: `base` is what both started from, `ours` the panel's,
/// `theirs` the server's. `None`: not there (a field left out, a list
/// entry removed).
fn merge(base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> Option<Value> {
    if ours == theirs || theirs == base {
        return ours.cloned();
    }
    if ours == base {
        return theirs.cloned();
    }
    // Both changed it.
    match (base, ours, theirs) {
        (Some(Value::Object(base)), Some(Value::Object(ours)), Some(Value::Object(theirs))) => {
            Some(Value::Object(merge_objects(base, ours, theirs)))
        }
        (Some(Value::Array(base)), Some(Value::Array(ours)), Some(Value::Array(theirs)))
            if [base, ours, theirs].iter().all(|list| keys(list).is_some()) =>
        {
            Some(Value::Array(merge_lists(base, ours, theirs)))
        }
        _ => ours.cloned(),
    }
}

fn merge_objects(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = Map::new();
    // Ours first, then any field only theirs has.
    for key in ours.keys().chain(theirs.keys().filter(|key| !ours.contains_key(*key))) {
        if let Some(value) = merge(base.get(key), ours.get(key), theirs.get(key)) {
            merged.insert(key.clone(), value);
        }
    }
    merged
}

/// Each entry's `id`, if every entry is an object with one, and no two
/// share it.
fn keys(list: &[Value]) -> Option<Vec<&Value>> {
    let ids: Vec<&Value> = list.iter().map(|entry| entry.get("id")).collect::<Option<_>>()?;
    let unique = ids.iter().enumerate().all(|(at, id)| !ids[..at].contains(id));
    unique.then_some(ids)
}

fn find<'a>(list: &'a [Value], id: &Value) -> Option<&'a Value> {
    list.iter().find(|entry| entry.get("id") == Some(id))
}

/// Lists of things with ids, matched by id. The order is the panel's if
/// it reordered them, otherwise the server's; added entries go after the
/// one they followed.
fn merge_lists(base: &[Value], ours: &[Value], theirs: &[Value]) -> Vec<Value> {
    let ids =
        |list: &[Value]| -> Vec<Value> { list.iter().filter_map(|entry| entry.get("id").cloned()).collect() };
    let (base_ids, our_ids, their_ids) = (ids(base), ids(ours), ids(theirs));
    // Did the panel reorder what was there before?
    let kept_order = |list: &[Value]| -> Vec<Value> {
        list.iter().filter(|id| base_ids.contains(id) && our_ids.contains(id)).cloned().collect()
    };
    let reordered = kept_order(&our_ids) != kept_order(&base_ids);
    let (first, second) = if reordered { (&our_ids, &their_ids) } else { (&their_ids, &our_ids) };
    let mut order: Vec<Value> = first.clone();
    for (at, id) in second.iter().enumerate() {
        if order.contains(id) {
            continue;
        }
        // After the nearest one before it that's placed, or first.
        let after =
            second[..at].iter().rev().find_map(|before| order.iter().position(|placed| placed == before));
        order.insert(after.map_or(0, |after| after + 1), id.clone());
    }
    order.iter().filter_map(|id| merge(find(base, id), find(ours, id), find(theirs, id))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::cards::Card;
    use serde_json::json;

    fn card(id: &str, enabled: bool) -> Card {
        Card { id: id.into(), name: id.into(), enabled, nodes: Vec::new(), wires: Vec::new() }
    }

    fn config(cards: Vec<Card>) -> AppConfig {
        AppConfig { cards, ..AppConfig::default() }
    }

    fn enabled(config: &AppConfig) -> Vec<(&str, bool)> {
        config.cards.iter().map(|card| (card.id.as_str(), card.enabled)).collect()
    }

    #[test]
    fn a_change_elsewhere_joins_the_panels_own() {
        let old = config(vec![card("a", false), card("b", true)]);
        let ours = config(vec![card("a", true), card("b", true)]);
        let new = config(vec![card("a", false), card("b", false)]);
        let rebased = rebase(&ours, &old, &new, &Catalog::default());
        assert_eq!(enabled(&rebased), [("a", true), ("b", false)]);
        // Nothing changed elsewhere, or nothing here: one side as it is.
        assert_eq!(rebase(&ours, &old, &old, &Catalog::default()), ours);
        assert_eq!(rebase(&old, &old, &new, &Catalog::default()), new);
    }

    #[test]
    fn the_same_field_changed_on_both_sides_keeps_the_panels() {
        let old = config(vec![card("a", false)]);
        let mut ours = old.clone();
        ours.cards[0].name = "Ours".into();
        let mut new = old.clone();
        new.cards[0].name = "Theirs".into();
        new.cards[0].enabled = true;
        let rebased = rebase(&ours, &old, &new, &Catalog::default());
        assert_eq!((rebased.cards[0].name.as_str(), rebased.cards[0].enabled), ("Ours", true));
    }

    #[test]
    fn cards_added_and_removed_on_either_side() {
        let old = config(vec![card("a", true), card("b", true), card("c", true)]);
        // The panel added "x" after "a" and removed "c"; elsewhere "b" was
        // switched off and "y" added at the end.
        let ours = config(vec![card("a", true), card("x", true), card("b", true)]);
        let new = config(vec![card("a", true), card("b", false), card("c", true), card("y", true)]);
        let rebased = rebase(&ours, &old, &new, &Catalog::default());
        assert_eq!(enabled(&rebased), [("a", true), ("x", true), ("b", false), ("y", true)]);

        // Removed elsewhere, unchanged here: gone. Changed here: kept.
        let new = config(vec![card("a", true), card("c", true)]);
        let rebased = rebase(&ours, &old, &new, &Catalog::default());
        assert_eq!(enabled(&rebased), [("a", true), ("x", true)]);
        let mut changed = ours.clone();
        changed.cards[2].enabled = false;
        let rebased = rebase(&changed, &old, &new, &Catalog::default());
        assert_eq!(enabled(&rebased), [("a", true), ("x", true), ("b", false)]);
    }

    #[test]
    fn the_panels_reordering_stays() {
        let old = config(vec![card("a", true), card("b", true), card("c", true)]);
        let ours = config(vec![card("c", true), card("a", true), card("b", true)]);
        let new = config(vec![card("a", false), card("b", true), card("c", true)]);
        let rebased = rebase(&ours, &old, &new, &Catalog::default());
        assert_eq!(enabled(&rebased), [("c", true), ("a", false), ("b", true)]);
    }

    #[test]
    fn lists_without_ids_are_taken_whole() {
        let base = json!({ "list": [1, 2], "other": 1 });
        let ours = json!({ "list": [1, 2, 3], "other": 1 });
        let theirs = json!({ "list": [2], "other": 2 });
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!({ "list": [1, 2, 3], "other": 2 }))
        );
        // A field only one side has.
        let theirs = json!({ "list": [1, 2], "other": 1, "new": true });
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!({ "list": [1, 2, 3], "other": 1, "new": true }))
        );
    }
}
