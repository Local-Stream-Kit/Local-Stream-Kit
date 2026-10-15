# Example plugins

To try one, copy its folder into `plugins/` next to `lsk.json` (in the repo
root when running from source) and press **Look for new plugins** on the
panel's Plugins tab. Its nodes then show up in the card editor's add-node
menu. How to write your own is in
[docs/writing-a-plugin.md](../../docs/writing-a-plugin.md).

- **confetti**: a Confetti effect node and the overlay page that draws it.
  No server code: when the node fires, its page gets the node's values.
- **counter**: a Count effect node and a Counted event node, with server
  code in Rust (`src/lib.rs`) that keeps the counts between streams, and
  an overlay page that shows a count when it changes. Build its
  `server.wasm` first, from the repo: `cargo plugin examples/plugins/counter`
  (needs `rustup target add wasm32-wasip2`). Only `plugin.json`,
  `server.wasm` and `overlay/` are needed in `plugins/counter`.

- **bits-to-money**: a Bits to money value node (server code in Rust):
  what a cheer's bits are worth at the rate you set, as a number and as
  text ("$5.00"). Build it with `cargo plugin examples/plugins/bits-to-money`.
  Only `plugin.json` and `server.wasm` are needed in `plugins/bits-to-money`.

- **copypasta**: a Copypasta event node (server code in Rust) that hears
  the chat and fires when several viewers post the same message within
  a while; each node on a card has its own Viewers, Within and Shortest
  message. Build it with `cargo plugin examples/plugins/copypasta`, then
  allow it to hear the chat on the Plugins page.

A program can also be a plugin from outside, on the control API:
[examples/programs/fake-platform.mjs](../programs/fake-platform.mjs) brings
a "Fake membership" event node and a "Print" effect node. Run it with
`node examples/programs/fake-platform.mjs "<connection URL>"` (the URL from
Status → Control API).
