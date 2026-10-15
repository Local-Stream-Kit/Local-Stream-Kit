//! Works out the order transformers run in, so every producer of a signal
//! kind runs before the transformers that consume it.

use std::collections::BTreeSet;

use crate::model::SignalName;

pub struct Node<'a> {
    pub name: &'a str,
    pub produces: &'a [SignalName],
    pub consumes: &'a [SignalName],
}

#[derive(Debug, PartialEq, Eq)]
pub struct Order {
    /// Indices into the input, in run order.
    pub run_order: Vec<usize>,
    /// `(transformer, signal)` pairs where nothing else produces the signal.
    pub unproduced: Vec<(String, SignalName)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("transformers form a loop through their signals: {}", .0.join(", "))]
pub struct LoopError(pub Vec<String>);

/// Topological sort on produces → consumes. A transformer never receives its
/// own signals, so a transformer consuming what it produces isn't a loop.
/// Ties keep the original order, so the result is deterministic.
pub fn run_order(nodes: &[Node<'_>]) -> Result<Order, LoopError> {
    let feeds = |from: usize, to: usize| {
        from != to && nodes[from].produces.iter().any(|signal| nodes[to].consumes.contains(signal))
    };

    let mut unproduced = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        for signal in node.consumes {
            let produced = nodes
                .iter()
                .enumerate()
                .any(|(other, producer)| other != index && producer.produces.contains(signal));
            if !produced {
                unproduced.push((node.name.to_string(), *signal));
            }
        }
    }

    let mut remaining: BTreeSet<usize> = (0..nodes.len()).collect();
    let mut run_order = Vec::with_capacity(nodes.len());
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .copied()
            .find(|&candidate| !remaining.iter().any(|&other| feeds(other, candidate)));
        match ready {
            Some(index) => {
                remaining.remove(&index);
                run_order.push(index);
            }
            None => {
                return Err(LoopError(
                    remaining.iter().map(|&index| nodes[index].name.to_string()).collect(),
                ));
            }
        }
    }
    Ok(Order { run_order, unproduced })
}

#[cfg(test)]
mod tests {
    use super::*;
    use SignalName::*;

    fn node<'a>(name: &'a str, produces: &'a [SignalName], consumes: &'a [SignalName]) -> Node<'a> {
        Node { name, produces, consumes }
    }

    #[test]
    fn producers_run_before_consumers() {
        // "hype" consumes what "events" and "chat" produce, but is listed first.
        let nodes = [
            node("hype", &[Custom], &[Support, EmotesPosted]),
            node("events", &[Support], &[]),
            node("chat", &[EmotesPosted], &[]),
        ];
        let order = run_order(&nodes).unwrap();
        assert_eq!(order.run_order, [1, 2, 0]);
        assert!(order.unproduced.is_empty());
    }

    #[test]
    fn keeps_original_order_when_independent() {
        let nodes = [node("a", &[Support], &[]), node("b", &[Raid], &[]), node("c", &[Custom], &[])];
        assert_eq!(run_order(&nodes).unwrap().run_order, [0, 1, 2]);
    }

    #[test]
    fn consuming_own_kind_is_not_a_loop() {
        let nodes = [node("self", &[Custom], &[Custom])];
        let order = run_order(&nodes).unwrap();
        assert_eq!(order.run_order, [0]);
        assert_eq!(order.unproduced, [("self".to_string(), Custom)]);
    }

    #[test]
    fn detects_loops() {
        let nodes =
            [node("ok", &[Support], &[]), node("a", &[Raid], &[Custom]), node("b", &[Custom], &[Raid])];
        assert_eq!(run_order(&nodes), Err(LoopError(vec!["a".into(), "b".into()])));
    }

    #[test]
    fn reports_unproduced_signals() {
        let nodes = [node("combo", &[Custom], &[EmotesPosted])];
        assert_eq!(run_order(&nodes).unwrap().unproduced, [("combo".to_string(), EmotesPosted)]);
    }
}
