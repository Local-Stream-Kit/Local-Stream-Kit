# Control API

(There's a Stream Deck plugin built on this: `streamdeck/README.md`.)

Lets a Stream Deck, a hotkey tool (AutoHotkey, Bitfocus Companion…) or a
script switch cards and effects, run the queue and fire Custom events.
It lives on the app's own port (the overlay port, 5750 unless changed),
under `/control/`. The panel's **Status → Control API** box (a page until 2026-10-14) shows the
key, installs the Stream Deck plugin and makes ready-to-copy URLs.

## The key

Every request needs the key. Browsers let any web page send requests to
`localhost`, so without a key any site you visit could switch your cards.
Give it one of these ways:

- in the URL: `?key=…` (simplest; what the panel's URLs use)
- a header: `X-Control-Key: …`
- a header: `Authorization: Bearer …`

Only `localhost` or an IP address reaches the app (any other host name
gets `403`, against DNS rebinding), so use `http://localhost:5750` or
`http://127.0.0.1:5750`, not the PC's name. The app listens on
127.0.0.1 only; a Stream Deck (or its phone app) reaches it through the
Stream Deck software on this PC.

The key is made on first start and kept in `lsk_control.json`
next to the config. Treat it like a password: keep it off stream. "Make a
new key" on the panel's page replaces it; whatever used the old one gets
`401` until it's given the new one.

## Web requests

GET or POST, whichever your tool sends. Stream Deck's built-in **Website**
action works: tick "GET request in background". Don't open these URLs in a
browser tab: browsers sometimes load a typed URL ahead of time, which can
fire it twice.

| Request | Does | Answers |
|---|---|---|
| `/control/` or `/control/state` | nothing | the [state](#the-state) |
| `/control/cards/{id}/on`, `/off`, `/toggle` | switches a card | `{"id", "name", "on"}` |
| `/control/effects/{id}/on`, `/off`, `/toggle` | switches an effect | `{"id", "name", "on"}` |
| `/control/queue/pause`, `/resume`, `/toggle`, `/skip`, `/clear` | runs the queue | `{"paused", "playing", "waiting", "by_kind", "players", "on_stage", "up_next"}` |
| `/control/items/{id}/skip` | skips one queue item | `{"id", "was"}` |
| `/control/items/{id}/next` | plays one waiting item next | `{"id", "was"}` |
| `/control/mute/on`, `/off`, `/toggle` | mutes or unmutes every sound | `{"volume", "muted"}` |
| `/control/history/seen` | marks every event in the history seen | `{"new"}` |
| `/control/events/{name}` | fires a Custom event | `{"fired": "name"}` |

- **Cards** are found by id (it stays the same when a card is renamed; the
  panel's URLs use it), or else by name, ignoring case, when only one card
  has that name. Switching a card off takes what it has waiting out of
  the queue; on again before the queue gets there, it comes back (and a
  card switched on adds what it would queue for events still waiting).
- **Effects:** `rain`, `alert`, `sound`, `speech` today: the built-in
  Emote rain, Alerts, Sounds and Speak plugins' switches; `speech` is
  every voice read aloud, alerts' too (the Speak plugin's switch since
  2026-10-14; the app's own "Read aloud" before). Take the list from the state rather than knowing it: other
  plugins may join it. An effect switched off takes what it has waiting
  out of the queue (speech: alerts waiting lose their read-aloud
  message), and on again brings it back, as a card's switch does.
- **Queue:** `toggle` pauses a running queue and resumes a paused one.
  `skip` ends what's playing now; `clear` drops everything waiting, or
  with `?kind=follow` only that kind (the kinds are in the state:
  `sub`, `gift_bomb`, `bits`, `raid`, `reward`, `follow`, `hype_train`,
  `support`, and `other` for chat read aloud and the like).
- **One item** (2026-10-13), by the id the state lists it with
  (`queue.on_stage`, `queue.up_next`): on stage, it ends, as `skip`;
  waiting, it's dropped and never plays. One that's gone by the time the
  request arrives (played, its time ran out, cleared, skipped already) is
  left alone, so the item now in its place plays on. That isn't an
  error: the answer's `was` says what happened, `on_stage`, `waiting` or
  `gone`. An id that isn't a number is `404`.
- **Play one next** (2026-10-16), by its id in the same way: a waiting
  item moves to the front of the queue and plays once what's on stage
  ends (or first, when a paused queue resumes). The answer's `was` is
  `waiting` (it's at the front now, or was already), `on_stage` (it's
  playing: nothing changed) or `gone` (nothing changed).
- **Mute** (`on` mutes) silences every sound on the overlays, live: what
  plays goes on silently, so unmuting in the middle of a spoken message
  brings back the rest of it. It's the master volume's mute (Plugins →
  Sounds).
- **History seen** (2026-10-14) moves the History's seen marker above
  every event, as the panel's All seen: none are new any more. It works
  whether the panel shows the marker or not.
- **Events** run every card that starts with a Custom event node of that
  name (or with no name). Anything else in the query, and the fields of a
  JSON object sent as the body (POST), fill the card's placeholders:
  `/control/events/party?colour=red` makes `{colour}` say "red".

Errors come back as `{"error": "…"}` with a status: `401` (no key, or the
wrong one), `404` (no such card, effect or action, or an item id that
isn't a number), `400` (a body that isn't a JSON object), `503` (the app
is shutting down).

Examples:

```bash
curl -X POST "http://localhost:5750/control/cards/hydrate/toggle?key=YOURKEY"
```

```bash
curl -X POST -H "X-Control-Key: YOURKEY" -d '{"colour":"red"}' http://localhost:5750/control/events/party
```

## The state

```json
{
  "version": 1,
  "cards": [{ "id": "hydrate", "name": "Hydrate", "on": true }],
  "effects": [{ "id": "rain", "name": "Emote rain", "on": true }],
  "events": ["party"],
  "queue": {
    "paused": false, "playing": true, "waiting": 2,
    "by_kind": { "follow": 2 }, "players": 1,
    "on_stage": { "id": 41, "label": "Alert", "text": "Friend is raiding with 12 viewers", "kind": "raid" },
    "up_next": [
      { "id": 42, "label": "Alert", "text": "Bot1 followed", "kind": "follow" },
      { "id": 43, "label": "Alert", "text": "Bot2 followed", "kind": "follow" }
    ]
  },
  "kinds": [{ "id": "follow", "name": "Follows" }],
  "sound": { "volume": 0.8, "muted": false },
  "history": { "new": 4 }
}
```

- `cards` are in the Cards tab's order. A card that was deleted is simply
  not there.
- `events` are the names Custom event nodes listen for (on cards that are
  off too).
- `queue.by_kind` counts what's waiting by kind (kinds with none are left
  out); `kinds` lists every kind a clear takes, with its name.
- `queue.on_stage` is what's playing (`null` when nothing is), and
  `queue.up_next` what's waiting, in order: the first 100, as the panel
  lists them (`waiting` counts them all). Each has the `id` a skip or a
  play next takes, its node's title (`label`), what it shows or says
  (`text`) and its
  `kind`. Since 2026-10-13.
- `history.new` counts the events above the History's seen marker that
  its filter shows, replays left out (since 2026-10-14).
- `queue.players` counts overlays showing plugin pages (since
  2026-10-11; before, overlays that play alerts); with none, the queue
  waits.
- The `alert` effect is the built-in Alerts plugin's switch (2026-10-11).
- The `rain` effect is the built-in Rain plugin's switch (2026-10-11).
- The `sound` effect is the built-in Sounds plugin's switch (the Sound
  nodes), since 2026-10-11; alert sounds don't follow it (mute does).
- `version` goes up only when a change would break existing clients. New
  fields can appear at any time: ignore the ones you don't know.

## WebSocket

`ws://localhost:5750/control/ws?key=…` is for programs that stay connected
(the Stream Deck plugin). It sends `{"type": "state", "state": {…}}` on
connecting and again whenever the state changes, whoever changed it (the
panel, another client, the queue moving on).

Requests are messages too, each answered:

| Send | Answer |
|---|---|
| `{"type": "state"}` | `{"type": "state", "state": {…}}` |
| `{"type": "card", "id": "hydrate", "set": "toggle"}` | `{"type": "card", "card": {…}}` |
| `{"type": "effect", "id": "rain", "set": "off"}` | `{"type": "effect", "effect": {…}}` |
| `{"type": "queue", "action": "skip"}` | `{"type": "queue", "queue": {…}}` |
| `{"type": "queue", "action": "clear", "kind": "follow"}` | `{"type": "queue", "queue": {…}}` |
| `{"type": "skip_item", "id": 42}` | `{"type": "skipped_item", "id": 42, "was": "waiting"}` |
| `{"type": "play_next", "id": 43}` | `{"type": "moved_item", "id": 43, "was": "waiting"}` |
| `{"type": "mute", "set": "toggle"}` | `{"type": "sound", "sound": {…}}` |
| `{"type": "history_seen"}` | `{"type": "history", "history": {"new": 0}}` |
| `{"type": "event", "name": "party", "data": {"colour": "red"}}` | `{"type": "fired", "name": "party"}` |
| `{"type": "hello", "client": "streamdeck", "version": "0.2.0.0"}` | `{"type": "hello", "version": "0.1.1"}` |
| `{"type": "register", "plugin": {…}}` (its `icon`, if any, is the SVG itself) | `{"type": "registered", "plugin": "billybilly"}` |
| `{"type": "emit", "node": "membership", "values": {…}}` | `{"type": "emitted", "node": "membership"}` |
| `{"type": "emit", "node": "membership", "output": "ended", "values": {…}}` | `{"type": "emitted", "node": "membership"}` |

`hello` says which program this is and its version, once it's
connected; the answer has the app's version. The Stream Deck plugin sends
it with `client` `streamdeck` and its manifest's version, and the panel's
Status → Control API shows that version while it's connected, offering
**Update** when the plugin built into the app is newer. Other programs may
send one too (any name up to 64 characters, a version up to 32): it's
logged, and changes nothing else. A second hello replaces the first. A
hello without a name is answered with a `400` error. WebSocket only.

`register` and `emit` make the connection a plugin (the external
connection API): a program declares node types, fires its event nodes
(`output` fires another of a node's outputs of type `event` than `event`), and
is sent `{"type": "run", …}` when its effect nodes fire. A plugin with
`voices` is also sent `{"type": "speak", "request": 7, …}` and answers
`{"type": "spoken", "request": 7, "audio": "<base64>", "format": "wav"}`. See
[writing-a-plugin.md → Programs](writing-a-plugin.md#programs).

A request that fails is answered `{"type": "error", "status": 404,
"error": "…"}`, with the status a web request would have got.

## Later

- Plugins (Phase 5): plugin effects appear in `effects`; plugins may offer
  their own buttons, which would come as a `controls` list.
- Keys of their own for other programs, maybe with limits (read-only,
  events only). The key file already holds a list.
