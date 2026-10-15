# Stream Deck plugin

Keys for Local Stream Kit's [control API](../docs/control-api.md):

- **Card on / off**: switches a card, shows whether it's on, and is titled
  with the card's name (it follows renames; a deleted card's key says
  DELETED).
- **Effect on / off**: the same for emote rain, alerts, sounds and text to
  speech (and, later, plugins' effects: the list comes from the app).
  Switching a card or an effect off takes what it has waiting out of the
  queue; on again, it comes back if the queue hasn't got there yet.
- **Queue**: pause / resume (showing PAUSED or PLAYING), pause, resume, skip
  what's playing, clear what's waiting (everything or one kind, like
  "Clear follows", showing how many wait).
- **Mute**: mutes or unmutes every sound on the overlays, live (showing
  MUTED, or the master volume).
- **Mark history seen**: moves the History's "Seen up to here" line above
  every event (the panel's All seen), showing how many are new ("4 NEW",
  or ALL SEEN).
- **Fire event**: fires a Custom event; the cards that start with it run.

Keys follow changes made anywhere (the panel, other keys, scripts) over the
control API's WebSocket. A key's settings offer dropdowns filled from the
app, so nothing is typed.

## Install

```bash
cargo streamdeck
```

builds `streamdeck/dist/com.localstreamkit.streamdeck.streamDeckPlugin`
(it needs Node.js 24). Double-click it, and Stream Deck installs it.

Then drag a Local Stream Kit key onto your Stream Deck, and paste the line
from Local Stream Kit's **Status → Control API** box ("Stream Deck")
into the key's **Address**. Every key shares it, so it's pasted once.

## Develop

```bash
npm ci
```

```bash
npm run build
```

- `npm test`: what keys show (`src/view.ts`), the address, the images.
- `npm run try -- <address>`: runs the built plugin against a stand-in
  Stream Deck and a running Local Stream Kit, presses keys and checks what
  they show. It switches the first card and the rain, then switches them
  back; it doesn't press Mark history seen, which can't be undone.
- `npm run icons`: redraws the images in the `.sdPlugin` folder.
- `npm run watch` rebuilds and restarts the plugin on every change, once
  it's linked into Stream Deck (`npx streamdeck link
  com.localstreamkit.streamdeck.sdPlugin`).

The code:

- `src/plugin.ts` starts it: the actions, the app connection, the settings
  page's messages.
- `src/app.ts` is the connection to Local Stream Kit.
- `src/actions.ts` holds the kinds of keys.
- `src/view.ts` and `src/keys.ts` decide what a key shows, and draw it.
- `src/pi.ts` with `ui/settings.html` is a key's settings page. It's
  written by hand, not with sdpi-components, so it needs no download and
  can show the connection.
- `src/protocol.ts` is the control API's JSON. Keep it in step with
  `crates/protocol/src/control.rs`.

The plugin's UUID (`com.localstreamkit.streamdeck`) can't change without
breaking people's keys.
