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

Local Stream Kit has the plugin built in: on **Status → Control API**,
click **Install** next to Stream Deck, and Stream Deck (on the PC the app runs
on) asks whether to install it. Each release also has the file,
`com.localstreamkit.streamdeck.streamDeckPlugin`, beside `lsk.exe`:
double-click it, and Stream Deck installs it.

Then drag a Local Stream Kit key onto your Stream Deck, and paste the
**Address** from the same Control API box into the key's **Address**.
Every key shares it, so it's pasted once.

The plugin has a version of its own, in its manifest (`0.2.0.0`), raised
only when the plugin changes. It tells the app its version when it
connects, and Status shows it ("0.2.0 connected"). When the app has a
newer one built in, Status offers **Update**, which installs it the same
way. The first plugin, 0.1.0.0,
doesn't say its version, so Status doesn't show it: Install replaces it.

From the source:

```bash
cargo streamdeck
```

builds `streamdeck/dist/com.localstreamkit.streamdeck.streamDeckPlugin`
(it needs Node.js 24): the tests, then the file. `cargo dist` builds it
too, and into the app.

## Develop

```bash
npm ci
```

```bash
npm run build
```

- `npm test`: what keys show (`src/view.ts`), the address, the images,
  the hello.
- `npm run pack`: builds, then packs the file (`scripts/pack.mjs`): the
  plugin folder is copied to `packing/` with `LICENSE.md` beside the
  manifest, so the folder in the repo doesn't change.
- When you change the plugin, raise `Version` in its manifest: Status
  offers the update by it, and a release with a changed plugin and the
  same version is refused.
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
- `src/app.ts` is the connection to Local Stream Kit. It says hello with
  the plugin's version (Stream Deck's, from the manifest) when it
  connects.
- `src/actions.ts` holds the kinds of keys.
- `src/view.ts` and `src/keys.ts` decide what a key shows, and draw it.
- `src/pi.ts` with `ui/settings.html` is a key's settings page. It's
  written by hand, not with sdpi-components, so it needs no download and
  can show the connection.
- `src/protocol.ts` is the control API's JSON. Keep it in step with
  `crates/protocol/src/control.rs`.

The plugin's UUID (`com.localstreamkit.streamdeck`) can't change without
breaking people's keys.
