//! Plugins' tabs in the panel (the manifest's `tab`): what their pages send
//! their server code (`on-tab`), and what it sends them back
//! (`send-to-tab`), to every open panel. A message for code that isn't
//! running, or for a panel not showing the tab, is dropped.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;

/// The panel's stylesheet for tab pages (`protocol::plugin::TAB_STYLE_PATH`).
pub const TAB_STYLE: &str = include_str!("tab.css");

/// Messages waiting to be read, each way, before the slowest reader misses
/// some.
const BACKLOG: usize = 256;

/// A message between a plugin's tab and its server code.
#[derive(Debug, PartialEq)]
pub struct TabMessage {
    pub plugin: String,
    pub data: Value,
}

#[derive(Clone)]
pub struct PluginTabs {
    to_code: broadcast::Sender<Arc<TabMessage>>,
    to_panels: broadcast::Sender<Arc<TabMessage>>,
}

impl Default for PluginTabs {
    fn default() -> Self {
        PluginTabs { to_code: broadcast::channel(BACKLOG).0, to_panels: broadcast::channel(BACKLOG).0 }
    }
}

impl PluginTabs {
    /// A tab page's `send`, for its plugin's server code.
    pub fn to_code(&self, plugin: &str, data: Value) {
        let _ = self.to_code.send(Arc::new(TabMessage { plugin: plugin.to_string(), data }));
    }

    /// What tabs send, for server code (each copy keeps its own plugin's).
    pub fn code_listens(&self) -> broadcast::Receiver<Arc<TabMessage>> {
        self.to_code.subscribe()
    }

    /// Server code's `send-to-tab`, for every open panel.
    pub fn to_panels(&self, plugin: &str, data: Value) {
        let _ = self.to_panels.send(Arc::new(TabMessage { plugin: plugin.to_string(), data }));
    }

    /// What server code sends tabs, for an open panel.
    pub fn panel_listens(&self) -> broadcast::Receiver<Arc<TabMessage>> {
        self.to_panels.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_go_to_whoever_listens_each_way() {
        let tabs = PluginTabs::default();
        // Nobody listening yet: dropped, not kept.
        tabs.to_code("counter", Value::from(1));
        let (mut code, mut panel, mut other_panel) =
            (tabs.code_listens(), tabs.panel_listens(), tabs.panel_listens());
        tabs.to_code("counter", Value::from(2));
        tabs.to_panels("counter", Value::from(3));
        assert_eq!(*code.try_recv().unwrap(), TabMessage { plugin: "counter".into(), data: Value::from(2) });
        assert!(code.try_recv().is_err());
        for panel in [&mut panel, &mut other_panel] {
            assert_eq!(panel.try_recv().unwrap().data, Value::from(3));
        }
        assert!(tabs.to_panels.len() <= BACKLOG);
        assert!(!TAB_STYLE.contains("@import"), "a tab page's policy lets it load nothing else");
    }
}
