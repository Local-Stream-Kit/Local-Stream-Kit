# Writing a plugin

For people writing a plugin, and the AI helping them: everything a plugin
can declare, every message it gets and sends, and how to build, try and
share one. Give an AI this file, plus
[`crates/protocol/wit/plugin.wit`](../crates/protocol/wit/plugin.wit) for
server code, and one of the examples below.

## Contents

- [What a plugin can be](#what-a-plugin-can-be)
- [The folder](#the-folder)
- [Trying it out](#trying-it-out)
- [When it doesn't work](#when-it-doesnt-work)
- [Sharing it](#sharing-it)
- [The manifest](#the-manifest)
- [Nodes](#nodes)
- [Settings](#settings)
- [Overlay pages](#overlay-pages)
- [A tab in the panel](#a-tab-in-the-panel)
- [Server code](#server-code)
- [Programs](#programs)
- [Examples to start from](#examples-to-start-from)
- [Not possible yet](#not-possible-yet)

## What a plugin can be

A plugin adds **node types** to the streamer's cards (the node graphs that
say what reacts to what). **Event nodes** start a card ("a membership on
BillyBilly"); **effect nodes** end one ("confetti on the overlay"). A plugin
does its work in one of three ways:

| Kind | Files | Good for | Example |
|---|---|---|---|
| An overlay page | `plugin.json`, `overlay/` | Effect nodes that draw or play something on the overlay. No build step: HTML and JavaScript. | `examples/plugins/confetti` |
| Server code | `plugin.json`, `server.wasm`, maybe `overlay/` | Event nodes (a platform, a detector), web requests, WebSockets, timers, values kept between streams, text to speech voices. A WebAssembly component, sandboxed. | `examples/plugins/counter` |
| A program | your own program, nothing installed | Anything, in any language, without a sandbox: it connects to the app's control API and is a plugin while it runs. | `examples/programs/fake-platform.mjs` |

With server code, the effect nodes' runs go to the server code, and the
server code decides what the page shows (`send-to-page`). Without it, they
go straight to the page.

A plugin in a folder can also have a **tab of its own in the panel**: a
page that talks to its server code, for what the streamer does during a
stream (a list of requests to approve, counts to set right). See
[A tab in the panel](#a-tab-in-the-panel).

## The folder

```
plugins/                  next to lsk.json, in the folder the app runs from
  my-plugin/              the folder's name is the plugin's id
    plugin.json           the manifest (required)
    overlay/              served at /plugins/my-plugin/ (optional)
      index.html          the overlay page
      tab.html            a tab in the panel, if it has one (any name)
      …                   its scripts, styles, pictures, sounds
    server.wasm           server code (optional)
```

Nothing outside `overlay/` is served. The app checks these when it loads a
plugin:

- Effect nodes need `overlay/index.html` or `server.wasm`.
- Event nodes and voices need `server.wasm` (or a program instead of a
  folder).
- An effect node that waits in the queue needs `overlay/index.html` or
  `server.wasm` (whichever says when it's done: the server code, if
  there is some).

## Trying it out

- **Load it:** put the folder in `plugins/`, then press **Look for new
  plugins** at the end of the panel's Plugins tab. No restart. Its nodes
  are then in the card editor's add-node menu, under Events or Effects.
- **After changing `plugin.json` or `server.wasm`:** press Look for new
  plugins again. Server code is started again when either file changed.
- **After changing a page file:** refresh the overlay (the Browser Source's
  Refresh, or reload the browser tab). Plugin files aren't cached.
- **Fire it:** add the node to a card and press **Test this card**, or,
  with a `preview` in the manifest, press ▶ Play on the plugin's box on the
  Plugins tab.
- **Watch it:** open the overlay's link (Status → Overlays → Copy link) in
  Chrome or Edge rather than OBS, and use the developer tools. The page is
  the iframe with the id `plugin-<id>`; pick it in the console's context
  menu to see its logs. To inspect it inside OBS, start OBS with
  `--remote-debugging-port=9222` and open `http://localhost:9222` in Chrome.
- **Permissions:** a plugin with `hosts`, `page_hosts` or `hears` waits on
  its Plugins box until you press Allow (see [Permissions](#permissions)).

## When it doesn't work

- **It isn't in the add-node menu:** it didn't load. The Plugins tab lists
  folders that didn't load with every reason ("node \"burst\": an effect
  needs an event input (a trigger)"), and so does the app's console
  window. The header's orange Plugins dot shows while one needs a look.
- **Its server code stopped:** its Plugins box says why ("took longer than
  a second", "crashed: …", or "doesn't fit this app's plugin interface":
  rebuild it against the current `plugin.wit`, see Server code). Its nodes stay in the cards but do nothing.
  Switch the plugin off and on (or restart the app) to start it again.
- **Its node is "my-plugin: burst" with a problem:** the plugin isn't
  installed, or no longer has that node id. The node keeps its settings
  and comes back when the plugin does.
- **Server code logs:** `host.log` lines and anything written to stdout or
  stderr appear in the app's console window and its log files (`logs/`
  next to `lsk.json`), marked with the plugin's id. Warnings and errors
  are also listed at the bottom of Status. Debug lines are hidden
  unless **Detailed log** is ticked there (or the app is started with
  `RUST_LOG=info,lsk::plugins=debug`).
- **Nothing shows on the overlay:** check that the overlay shows the
  plugin (Status → Overlays: "Every effect", or the plugin ticked), that
  the plugin is switched on, and the page's console (see Watch it above).

## Sharing it

- Zip the plugin: `plugin.json` at the top of the zip, or in one folder
  inside it (zipping the folder itself does that, with any zip tool,
  `Compress-Archive` included). Include `plugin.json`,
  `overlay/` and `server.wasm`; leave out source code and build output. At
  most 64 MB unpacked.
- Users install it with **Install from a zip** on the Plugins tab, or by
  dropping the zip on that page. It takes effect at once, and replaces an
  installed plugin with the same id, so raise `version` with each release.
- Users trust a plugin by installing it. Overlay pages are sandboxed and
  server code only reaches the hosts the user allowed; a program has no
  sandbox.
- An update that lists a new host has to be allowed again.

## The manifest

`plugin.json`. Unknown keys are ignored.

```json
{
  "id": "confetti",
  "name": "Confetti",
  "version": "0.1.0",
  "api": 1,
  "author": "Your name",
  "description": "Bursts of confetti on the overlay.",
  "nodes": [
    {
      "id": "burst",
      "title": "Confetti",
      "category": "effects",
      "inputs": [
        { "name": "trigger", "label": "Trigger", "type": "event" },
        { "name": "pieces", "label": "Pieces", "type": "number" }
      ],
      "settings": [
        { "key": "pieces", "label": "Pieces", "type": "number", "default": 150, "min": 1, "max": 1000 },
        { "key": "colour", "label": "Colour", "type": "colour", "default": "#ffcc00" }
      ]
    }
  ]
}
```

| Key | | What it is |
|---|---|---|
| `id` | required | Lowercase letters, digits and `-`; the folder's name. Not `rain`, `alert`, `sound`, `chat`, `shell` or `plugin`. The id of a built-in plugin that isn't one of those (`media`, `particles`, `speak`, `hype-train`) replaces the built-in one. |
| `name` | required | Shown in the panel. |
| `version` | required | Any text; `"1.2.0"`. |
| `api` | required | `1`. A plugin for an API version the app doesn't know isn't loaded. |
| `author`, `description` | | Shown on its Plugins box. |
| `nodes` | | Its node types ([Nodes](#nodes)). |
| `settings` | | Settings for the whole plugin, set on its Plugins box (an API key, a channel, a look) ([Settings](#settings)). |
| `presets` | | `{ "label": "Style" }`: the plugin's `settings` become named sets the user makes (Duplicate, Delete, a name), and each node picks one ([Presets](#presets)). |
| `preview` | | A preview of the page on its Plugins box ([Preview](#preview)). |
| `icon` | | Its nodes' icon, drawn small in the text's colour beside their titles (as Twitch's and OBS's are): an SVG file in the plugin's folder (`"icon.svg"`), 24 × 24, one colour (only its shape counts). A program gives the SVG itself (`"<svg …>…</svg>"`). At most 16 KB. |
| `help` | | A page file (`"help.html"`, a path under `overlay/`) shown on its Plugins box under "Help", without scripts, on white unless it sets a background. |
| `own_source` | | `true` for a page that needs a Browser Source of its own (as the chat display does): an overlay showing "every effect" leaves it out, and only one that names it shows it. |
| `hosts` | | Hosts its server code may reach ([Permissions](#permissions)). |
| `page_hosts` | | Hosts its overlay page may load from ([Permissions](#permissions)). |
| `hears` | | `["chat"]`: its page and its server code (or program) get every chat message; `["events"]`: its server code (or program) gets every event ([Permissions](#permissions)). |
| `moderates` | | `true`: its server code moderates the chat with the streamer's Twitch login, timing out and banning viewers and deleting messages (`host.moderate`, [Permissions](#permissions)). Only with server code. |
| `voices` | | Text to speech voices, `[{ "id": "beeps", "name": "Beeps" }]` ([Voices](#voices)). |
| `tab` | | A tab of its own in the panel, `{ "label": "Requests", "page": "tab.html", "count": true }` ([A tab in the panel](#a-tab-in-the-panel)). Not for a program. |

### Permissions

Each needs the user to press Allow on the plugin's Plugins box (Take back
undoes it). Until then:

- **`hosts`**: its server code doesn't run at all. The hosts its web
  requests and WebSockets may reach, redirects included: `discord.com`, or
  `*.example.com` for the subdomains of `example.com` (not `example.com`
  itself; list both if it needs both). Lowercase, no scheme or port: a
  listed host allows every port on it, so `127.0.0.1` lets it reach
  `ws://127.0.0.1:47600/`. Every port but the app's own: server code can't
  reach the app itself (its port on this PC, by any name or address;
  redirects too).
- **`page_hosts`**: its page can't load from them. Written as `hosts`;
  allowed, they're added to the page's Content-Security-Policy over https
  (scripts, styles, fonts, pictures, connections). For Google Fonts:
  `["fonts.googleapis.com", "fonts.gstatic.com"]`.
- **`hears`**: it doesn't hear what it lists. `"chat"`: its page gets
  every chat message ([Chat messages](#chat-messages)), and its server
  code's `on-chat` (or its program) too; `"events"`: its server code's
  `on-event` (or its program) gets every event.
- **`moderates`**: `host.moderate` refuses. Allowed, it still moderates
  only while the streamer is logged in with Twitch on Status, with
  Moderating ticked.

### Voices

A plugin with `voices` is a text to speech engine. Its voices are in every
voice picker under the plugin's name while it's on; chosen, a voice is
stored as `<plugin>:<voice id>` (voice ids: lowercase letters, digits, `-`,
`_`). The app asks its server code (the `speak` export) or its program (a
`speak` message) for each message, and the answer is a whole WAV, MP3 or Ogg
file, at most 8 MB, within 30 seconds. While the plugin isn't running, its
voices' messages are read in the app's own voice.

## Nodes

| Key | | What it is |
|---|---|---|
| `id` | required | Unique in the plugin: lowercase letters, digits, `-` and `_`. Cards store it, so don't rename it. |
| `title` | required | Shown on the node and in the add-node menu. |
| `category` | required | `"events"`, `"flow"`, `"values"` or `"effects"`. |
| `inputs` | | Sockets on the left. |
| `outputs` | | Sockets on the right. |
| `settings` | | Its fields ([Settings](#settings)). |
| `queue` | effects | `"never"` (the default), `"always"`, or `"optional"`: the node's tick box setting with the key `queue` decides ("Wait in the queue"; the node shows it under all its fields). See [The queue](#the-queue). |
| `describe` | effects | The input or setting whose value Activity shows for a queued run (an alert's message); else its title. |
| `changes` | effects | Keys of the plugin's own `settings` a node can change for itself: the node gets "+ Change a setting", and its runs bring `changes`. |
| `viewer` | effects | `true`: its runs bring the viewer who set it off as `values.viewer` ([Values](#values)); server code gets them as `run`'s `viewer`. No input or setting may then be named `viewer`. |

**Sockets** (`inputs`, `outputs`): `name` (lowercase letters, digits,
`_`), `label`, and `type`: `event`, `number`, `text` or `emotes`. One text
input of an effect node can have `"speak": true`: the app reads its value
aloud with the user's text to speech settings, the node gets
"+ Change a voice setting", and the value comes with the spoken message
(`speech`, see [Values](#values)).

**Event nodes:** no inputs. Their outputs must include one
named `event` of type `event`, which `emit` fires; more outputs of type
`event` ("ended", say, beside `event` for "started") are fired by name
with `emit-output` (a program: `emit` with `output`). Use the built-in events' names where they
fit, so cards read the same whatever the platform: `viewer` (text),
`amount` (number), `message` (text), `summary` (text, a sentence like
"Ann subscribed for 3 months"; the node's title when none is sent). Server
code or a program fires them. A `text` setting named after one of its
text outputs is a filter, applied by the app: it lets through only
events whose value is one of the setting's (ignoring case; empty: any).
With `"many": true` it holds several, picked one by one on the node
(OBS's Scene changed: Scenes, with `suggest_from` `"scenes"` and the
placeholder "Any scene"). Its other settings, of any kind (`many` too,
on a text), are for your server code: `nodes-changed` tells it every
copy of its event nodes on the cards with their settings, and
`host.emit-to` fires one copy (a detector that fires one card at three
viewers and another at ten). A program gets `nodes` and fires one copy
with `emit` and `to` ([Programs](#programs)).

**Effect nodes:** at least one input of type `event` (the trigger; `input`
in a run says which one fired). Their outputs, if any, are `event`s your
server code fires for a run with `host.carry-on` (a scene switch's
Started once OBS takes it, Ended once it's on air): the card carries on
from there with the event that set the run off. Only with server code;
pages and programs can't fire outputs yet. While a node waits in the
queue (`"queue": "always"`, or `"optional"` with its tick box ticked) the
app gives it two event outputs of its own, **Started** and **Ended**
(`started`, `ended`): they fire when its run goes on stage and when your
page (or server code) says it's `done` (or its time runs out), not when
it's skipped. You don't declare or handle them; your page just says
`done` as before. A
node that can wait in the queue can't have outputs of its own named
`started` or `ended`.

**Flow nodes** (server code or a program): at least one input and one
output of type `event`. An event coming into one waits while your server
code's `route` says where it goes: on from one of its event outputs
(`go`), with what its other outputs (number, text, emotes) carry for that
event; nowhere (`stop`); or `later`, answered with `host.route-later`
within an hour (after a web request, say). A program answers `route`
with `routed`, within an hour ([Programs](#programs)). The card doesn't
wait: other events go through it meanwhile, and one that waits may be
overtaken by later ones, as after a Delay. A wire from a flow node's
value output gives nothing for an event that didn't go through it (the
input's own field is used). A flow node always gets the viewer, so it
needs no `viewer`; `queue`, `changes` and `"speak"` are for effect nodes.
While its plugin is off, its server code isn't running or its program
isn't connected, nothing gets through; events still waiting for an
answer when your server code stops (switched off, updated, crashed) or
your program goes go nowhere. A number your code gives an output is
finite: NaN or infinity gives nothing.

**Value nodes** (server code or a program): no inputs or outputs of type
`event`, at least one output. When an event needs one of its outputs
(an input wired from it, or from a Math or Text node fed by it), your
server code's `compute` gets the node's inputs and settings and answers
its outputs' values, by name: `now(values)`, or `later` and then
`host.compute-later` within 10 seconds (after that the node gives
nothing, and the inputs wired from it use their own fields). A program
answers `compute` with `computed`, within the same 10 seconds. It's
asked once per event; value nodes wired into each other are asked in
turn, the one furthest back first. The event waits for the answer, the
card doesn't: other events go through meanwhile. While its plugin is
off, its server code isn't running or its program isn't connected, it
gives nothing, as it does at once for the events still waiting when your
server code stops or your program goes.

**Inputs with a field:** a setting with the same key and type as a
`number`, `text` or `emotes` input is that input's field while nothing is
wired into it, as on built-in nodes. Other settings get a row of their own,
above the inputs.

## Settings

The same format for a node's `settings` and the plugin's own `settings`.

| Key | What it is |
|---|---|
| `key` | Lowercase letters, digits and `_`; unique in the list. |
| `label` | Shown next to the field. A number's unit is added ("Lifetime (s)"). On a node the labels share a column, so the fields line up; a long one makes the node wider (its fields keep about 140 px), so keep them short. |
| `type` | One of the types below. |
| `default` | Its value until the user changes it (each type has one if left out). |
| `required` | `true` on a picture, media or sound setting: a node with none picked has a problem ("Pick a sound."), so its card doesn't run. |
| `when` | Node settings: `{ "wired": "emotes" }` or `{ "unwired": "emotes" }`: shown (and sent) only while that input is or isn't wired. Shown right under it. `{ "is": { "shape": "picture" } }`: shown (and sent) only while another of the node's `choice` settings has that value (several: all of them); it keeps its value while hidden. Not with `required`. The plugin's own settings: `{ "set": "picture" }`: shown on the Plugins box only while that other setting is set (a picture, sound or text picked or typed, a tick box ticked); it's still sent. |
| `group` | The plugin's own settings: a section on the Plugins box ("Look", "Physics") under its name, after the ones without. |
| `short` | The plugin's own settings: a shorter label for the Plugins box, where the group's heading says the rest (`"Size"` under Text, `label` being `"Text size"`, which nodes show). |
| `caption` | The plugin's own `bool`s and `number`s: words after the tick box or the field (`"As pictures"`, the label being `"Emotes"`; `"messages at most"`). A number without a slider has a short field. |
| `beside` | The plugin's own `bool`s: another of its settings' `key`. The tick box is then a small button, pressed while it's on, beside that setting's field (Bold beside Font). Not beside a `file` or `secret` setting. |
| `icon` | With `beside`: the button's face, `"bold"` (**B**) or `"italic"` (*I*); without one, its label. |

| `type` | Stored and sent as | Other keys | Where |
|---|---|---|---|
| `number` | a number, clamped to `min` / `max` | `min`, `max`; `unit`: `px`, `percent` (stored 0–1, shown 0–100 %), `ms` (stored in ms, shown in seconds), `metres_per_second_squared`; `slider`: `true` (needs `min` and `max`; on the Plugins box) with an optional `step` | both |
| `text` | on a node: a text value ([Values](#values)); the plugin's own: a plain string | `placeholder` (shown while empty); `suggestions`: a list (font names) shown as a dropdown, with "Other…" for anything else (empty first, as the placeholder or "None"; for the plugin's own, with a `default` and no placeholder, there's no empty choice and empty shows as the default); `suggest_from`: the name of a list of choices your server code fills in (`set-options`, or `set-choices` to show a label and store a value), offered the same way, after `suggestions`; `{key}` in it is another of the node's (or plugin's) settings' value, so `"sources/{scene}"` follows the Scene setting; `many`: several values (a list): on an event node's filters, picked one by one; the plugin's own, a line each with × and Add on the Plugins box, its `default` the items separated by commas (`"Nightbot, Moobot"`); not one a node changes | both |
| `bool` | `true` / `false` | | both |
| `colour` | `"#rrggbb"` | | both |
| `choice` | the picked option's `value` | `options`: `[{ "value": "top", "label": "The top" }]`; the default is the first unless named. On the Plugins box, up to four options whose labels add up to 32 characters at most are a row of buttons | both |
| `position` | one of the nine names `top_left`, `top`, `top_right`, `left`, `center`, `right`, `bottom_left`, `bottom`, `bottom_right` (default `top`), or anywhere between as `{ "x": 12, "y": 85 }`: see [Placing a position](#placing-a-position) | `random`: `true` adds "Anywhere (random)", sent as `"random"` | both |
| `picture` | its path on the app (`"/pictures/hype.gif"`), an `http(s)` address as it is, or `null`. The user can pick a folder of pictures instead: you get one of them, picked at random each time it's sent | `many`: `true` sends a list of paths instead: a picture is a list of one, a folder all its pictures (in name order), none `[]` | both |
| `media` | as a picture; a picture or a `.webm` / `.mp4` video | `many`, as a picture's | both |
| `sound` | its path on the app (`"/sounds/ding.mp3"`), or `null`; or one from a folder, as for a picture | `many`, as a picture's | both |
| `emotes` | stands in for the emotes input with the same key: stored as `"random"` (random emotes from the channel) or a list of codes (`["Kappa", "PogU"]`); sent as emotes | `count`: a number input or setting of the node: that many are picked at random; without it one random emote, or every listed code once | nodes |
| `preset` | the picked preset's id (empty: the first) | | nodes, one each, with `presets` |
| `file` | the picked look's text | `starts`: your looks, page files with names, `[{ "value": "looks/plain.css", "label": "Plain" }]`; `default`: the one picked while none is (`"looks/plain.css"`); `file`: a name whose extension the user's copies get (`"chat.css"`) | the plugin's own |
| `secret` | server code only: a plain string, `""` while none is saved. Pages never get it | `placeholder` (shown while none is saved) | the plugin's own |

A node's **text settings are templates** the user types: `{viewer}` and the
event's other values are filled in, `{e:Kappa}` is that emote, and
`[highlight]…[/]` marks a part with one of the app's text styles. The
plugin gets the result.

A **`file` setting** is a pick of looks: yours (`starts`, which never
change) and the user's copies. On the Plugins box, typing in one of yours
makes a copy ("Classic copy"), saved as
`plugin_data/<your id>/<key>/<name>.css` with Save, which any editor can
change too: the app checks the files once a second, and the page gets the
picked look's text in a `settings` message. Good for a stylesheet. If your
plugin has a `help` page, its ? sits beside the picker (about the markup
the stylesheet styles), not as a fold of its own.

A **`secret` setting** is for a password or an API key. It isn't kept in
`lsk.json` with the other settings but in `lsk_secrets.json` beside it,
and only your server code gets it (with `host.settings`); your page
doesn't, and the panel shows a password box that never shows it again
once saved. Read it when you need it: the user can change it while your
code runs. A plugin that replaces a built-in one (same id) doesn't get the
built-in's saved secrets; the user saves its own.

### Presets

With `"presets": { "label": "Style" }`, the plugin's own `settings` are
edited as named sets on its Plugins box. A node picks one with a setting of
type `preset`, and each run brings `settings`: every one of the plugin's
own settings from that preset, with the node's `changes` on top. The page's
`settings` message, and server code's `host.settings`, are the first
preset's.

### Preview

```json
"preview": { "node": "burst", "values": { "pieces": 300, "message": "Thanks, Ann!" } }
```

The page in a sandboxed iframe on the plugin's Plugins box, scaled to fit,
with its settings (again whenever they're edited). Once it has loaded it's
sent a run of `node` (an effect node), and again for **▶ Play**: `values`
are samples by input or setting name (a text as a plain string), the rest
the node's defaults. These runs have `preview`: `"show"` for the first
(draw it quietly: no sound) and `"play"` for Play. A page may keep a
preview run up and follow `settings` messages while it shows, so the
preview shows what's being edited, as the Alerts page does. **Show on
overlay** sends the same run to the overlays, without `preview`. `width` and `height` give the page's
size in OBS (1920 × 1080 unless given); with `shown_height`, only the
bottom of it that tall shows in the preview, for a page that fills from
the bottom (a chat). With `"samples": true` (and no
`node`), the page is sent `{ "type": "samples" }` once it has loaded and for
each Play, and draws samples of its own.

## Overlay pages

`overlay/index.html` and the files next to it, served at
`/plugins/<id>/`. Plain HTML and JavaScript need no build.

**Where it runs:**

- In a full-size iframe on the overlay (1920 × 1080 in a usual Browser
  Source), above the built-in layers, with `sandbox="allow-scripts"`: its
  scripts run, but its origin is opaque, so it can't reach the overlay, the
  panel or other plugins' pages, and **`localStorage`, `sessionStorage`,
  IndexedDB and cookies don't work**. To keep something, use page memory
  ([`keep`](#messages-a-page-sends)).
- **Keep the background transparent**, and don't set `color-scheme`: a
  different one makes Chromium paint the iframe opaque.
- **Size a canvas from the page on every frame** (`innerWidth`,
  `innerHeight`), not only once and on `resize`: the page's size can
  arrive after your script has run, with no `resize` event, and a canvas
  sized at the start then stays 0 × 0. The examples' `fit()` does this.
- It may load from the app only (relative paths are fine), plus `data:`,
  `blob:` and the allowed `page_hosts`. It can't call out to the internet:
  that's server code's job. It can't open the app's WebSockets either:
  everything it hears comes from the overlay page.
- Fetches from the app are cross-origin, and are allowed: its own files,
  emote images (the `url`s it's sent), pictures, sounds, spoken messages.
  So `ImageDecoder`, module scripts and WebAssembly work.
- Sound plays without a click in OBS; in a browser tab, click the page
  once first. Multiply every volume by the master volume (the `volume`
  message).
- Messages sent before the page has loaded wait for its `load` event.
  Then it gets the master volume, its settings, and what it kept (if
  anything), before runs.

**Listening:** take messages only from `window.parent` (other plugins'
pages could post to it too):

```js
addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "settings") settings = message.values;
  else if (message.type === "volume") master = message;
  else if (message.type === "run" && message.node === "burst") burst(message.values);
});
```

### Messages a page gets

Every one is an object with a `type`. On the overlay, all but `volume` also
have `issued_at_ms` (when the app sent it, Unix ms); in the preview they
don't.

| `type` | When | Fields |
|---|---|---|
| `run` | One of its effect nodes fired (for a queued node: its turn came) | `node`, `input` (the event input it came into), `values` ([Values](#values)), `changes` (only when the node changes some of the plugin's settings), `settings` (only with presets), `queue_id` (only for a queued run), `preview` (only in the panel's preview: `"show"` or `"play"`, see [Preview](#preview)), `origin` (only for a test's or a replay's run: `"test"` or `"replay"`) |
| `skip` | The queue skipped its queued run: stop showing it | `queue_id` |
| `settings` | When it loads, and whenever the plugin's own settings change | `values`: every setting by key, as in the settings table |
| `volume` | When it loads, and whenever the master volume or mute changes | `volume` (0–1), `muted` |
| `kept` | When it loads, if it kept something | `value` |
| `message` | Its server code called `send-to-page` | `data`: the JSON it sent |
| `samples` | Its preview's Play, with `"samples": true` | |
| `chat` | A chat message, with `"hears": ["chat"]` | `line` ([Chat messages](#chat-messages)) |
| `chat_removed` | A moderator deleted a message | `id`: its line's id |
| `chat_user_removed` | A viewer was banned or timed out: their lines go | `platform`, `user_id` |
| `chat_cleared` | The chat was cleared | `platform` (empty: every platform) |

A run of a node that reads aloud is sent once its spoken message is made.

### Messages a page sends

With `window.parent.postMessage(message, "*")`:

| Message | What it does |
|---|---|
| `{ "type": "done", "queue_id": 12 }` | A queued run has finished; the queue moves on. Only the page the run went to can end it (with server code, only the server code can: `host.done`). |
| `{ "type": "keep", "value": … }` | Keeps any JSON with the app (`null` forgets it; at most 256 KB). The page gets it back as `kept` when it loads, after its settings, so it carries on after a reload or a restart. Shared by every overlay showing the page; the last one kept wins. |

### Values

What `values` in a run holds: every input that isn't an event, and every
setting the node shows, by name: what's wired in, or else the setting.

| Type | As JSON |
|---|---|
| number | a number |
| text | `{ "text": "hi Kappa", "parts": [ … ] }` (see below), plus `speech` for an input read aloud |
| emotes | `[{ "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }]` |
| bool, colour, choice, position, preset | as in the settings table |
| picture, media, sound | a path on the app, or `null` |
| `viewer` (with `"viewer": true`) | `{ "platform": "twitch", "id", "name", "display_name", "color": "#rrggbb", "badges": [{ "set", "title", "image" }], "roles": { "broadcaster", "moderator", "vip", "member" } }`, or `null` for an event without one; `color` and `image` may be missing |

**A text** is the plain text, and the same as parts, so a page that only
wants text reads `.text`:

```json
{
  "text": "Ann subscribed! Kappa",
  "parts": [
    { "type": "text", "text": "Ann", "style": { "name": "highlight", "font": "", "size_percent": 100, "bold": true, "italic": null, "colour": "#ffcc00" } },
    { "type": "text", "text": " subscribed! " },
    { "type": "emote", "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }
  ],
  "speech": { "url": "/speech/….wav", "duration_ms": 2400, "volume": 0.8 }
}
```

- `overlays` are the paths of zero-width emotes drawn on top of it.
- `style` is on parts the user marked with a text style: an empty `font`
  or `colour`, or a `null` `bold` or `italic`, means as the text around it;
  `size_percent` is relative to it. Draw it however suits the page.
- `speech` is the spoken message to play: `volume` is the node's, so
  multiply it by the master volume. `duration_ms` is only known for WAV
  files. It's missing when nothing is read (text to speech off, an empty
  text, or a message that couldn't be made).

### The queue

A node with `"queue": "always"` (or `"optional"` with its tick box on)
waits its turn in the app's queue with the alerts, shown on the Activity
page with its title (and its `describe` value), and can be paused, skipped
and cleared there.

1. When its turn comes, the page gets the `run` with a `queue_id`.
2. The page shows it, then sends `{ "type": "done", "queue_id" }`.
3. On Skip, the page gets `{ "type": "skip", "queue_id" }`: stop at once;
   there's no need to send `done`.

- After an overlay reconnects, the run on stage is sent again with the same
  `queue_id`: ignore one the page is already showing.
- A page that never answers is given two minutes.
- The queue waits while no overlay shows the plugin's page.
- A run that's waiting is made again when the streamer changes the cards
  or the plugin's settings, so its values and `settings` are as they are
  when its turn comes, not as they were when its node fired; one whose
  card no longer queues it, or whose plugin was switched off meanwhile,
  doesn't come at all. Nothing to do about it:
  the page only ever gets the run when its turn comes.

With server code, the server code plays it instead (see
[Server code](#server-code)): `run` gets the `queue-id`, it calls
`host.done(queue-id)` when it has finished, and `skip(queue-id)` is
called on Skip (undo what stays on: hide the source it showed). The queue
then plays it while the server code runs, whether or not an overlay is
open, and waits while it doesn't (switched off or stopped). If it stops
mid-run, the run plays again from the start once it runs again.

### Chat messages

With `"hears": ["chat"]` (and the user's Allow), the page gets every chat
message, commands and bots included, so it leaves out what it doesn't want:

```json
{ "type": "chat", "line": {
  "id": "twitch:3f2a…", "platform": "twitch", "at_ms": 1760184000000,
  "source": { "type": "chat" },
  "user": { "id": "123", "name": "ann", "display_name": "Ann", "color": "#ff0000",
            "badges": [{ "set": "subscriber", "title": "6-Month Subscriber", "image": "https://…" }],
            "roles": { "broadcaster": false, "moderator": false, "vip": false, "member": true } },
  "parts": [ { "type": "text", "text": "hi " },
             { "type": "emote", "code": "Kappa", "sprite": { "image": { "source": "twitch", "id": "25" }, "overlays": [] } } ],
  "marks": { "action": false, "highlighted": false, "first": false, "reward": false, "cheer": false,
             "reply": true, "reply_to": { "user": "Bob", "text": "what's up" } } } }
```

A chat line's emote is a `sprite`, not a `url`: its image is at
`/emote/<source>/<id>` (and the same for each of `overlays`).

### Placing a position

A `position` setting says how far across and down the room your thing
moves in it goes, in %: 0 keeps it against the left (or top) edge, 100
against the right (or bottom), 50 centres it. The nine names are those
points (`top_left` is 0, 0; `top` 50, 0; `center` 50, 50; `bottom_right`
100, 100); anywhere else comes as `{ "x": 12, "y": 85 }`. Put the
thing's point that far across it on the room's point that far across,
so it never goes off screen and a bigger one grows away from the edges
it's against:

```js
const NAMES = { top_left: [0, 0], top: [0.5, 0], top_right: [1, 0], left: [0, 0.5], center: [0.5, 0.5],
                right: [1, 0.5], bottom_left: [0, 1], bottom: [0.5, 1], bottom_right: [1, 1] };
// Across and down, each 0–1.
function place(position) {
  if (typeof position === "string") return NAMES[position] || NAMES.top;
  return [position.x / 100, position.y / 100];
}
// The room: `position: relative`, inside your margin from the edges.
// The thing: `position: absolute; width: max-content`.
const [x, y] = place(settings.position);
thing.style.left = `${x * 100}%`;
thing.style.top = `${y * 100}%`;
thing.style.translate = `${-x * 100}% ${-y * 100}%`;
```

The app's own Rust pages use `protocol::plugin::Position` (`from_value`,
`fractions`).

### Pages written in Rust

A crate built with [trunk](https://trunkrs.dev): its output is the plugin's
`overlay/` folder, and its `public_url` is `/plugins/<id>/`. Its
WebAssembly starts after the page has loaded, when the page has already been
sent its settings, so keep early messages with a small inline script in
`index.html` and handle them first (see `crates/rain/index.html` and
`crates/rain/src/lib.rs`). Write your own serde types for the messages
above; the app's are in `protocol::plugin`, which isn't published.

## A tab in the panel

`"tab": { "label": "Requests", "page": "tab.html", "count": true }` gives
the plugin a tab in the panel's row, after Activity (plugins' tabs by
the plugins' names), while the plugin is switched on.

- `label`: the tab's name, 1 to 16 characters (the panel is often an OBS
  dock 380 px wide).
- `page`: one of its page files (a path under `overlay/`), served at
  `/plugins/<id>/tab.html`. It needs no `overlay/index.html`.
- `count` (optional): its server code puts a number on the tab
  (`host.set-tab-count`: "Requests (2)"), and the row keeps room for one.
- A row that's full puts the tabs at its end under ⋯ (More), the app's
  Plugins, Test and Status first, so in the dock a plugin's tab usually
  stays in the row; one with a number under ⋯ puts a dot on ⋯.

The page fills the panel under its header (at most 760 px wide) and
scrolls itself. It's hosted as an overlay page is: a sandboxed iframe
(scripts run; it can't reach the panel or other pages) with the same
policy (it loads only from the app, and the `page_hosts` the user
allowed). It's loaded when its tab is opened and gone when another page
is, so it asks its server code for what to show when it loads. While
its server code isn't running (stopped, or its hosts not allowed yet),
the panel says so above the page.

**The panel's look:** link the app's stylesheet,
`<link rel="stylesheet" href="/plugin-tab.css">`: the panel's colours as
CSS variables (`--ground`, `--panel`, `--text`, `--muted`, `--accent`,
`--ok`, `--warn`, `--error`, …), its page and type, plain buttons
(`class="primary"`, `"ghost"`, `"danger"`, `"small"`), fields, and
`.box`, `.row` and `.muted`. The font is the system's. Keep a `<select>`
out of it: the browser opens its list as a window of its own, which an
OBS dock on a screen with a different scaling draws in the wrong place;
use buttons or a list. A `<form>` doesn't submit in the sandbox: take
Enter in a field, and a button's click, yourself.

**Messages**, with `postMessage`, as an overlay page's (take them only
from `window.parent`):

| | Message |
|---|---|
| It gets | `{ "type": "settings", "values": … }` when it loads and when they change: the plugin-wide settings, as the overlay page gets them. |
| It gets | `{ "type": "message", "data": … }`: what its server code sent (`host.send-to-tab`), to the tab in every open panel. |
| It sends | `window.parent.postMessage({ type: "send", data }, "*")`: any JSON (at most 256 KB) for its server code, which gets it as `on-tab(json)`. Dropped while the code isn't running. |

With two panels open (a browser and an OBS dock), each tab's `send`
reaches the code, and its answers reach both tabs. The Counter example's
tab (`examples/plugins/counter/overlay/tab.html`) lists its counts with
− and +.

## Server code

`server.wasm`: a WebAssembly component (WASI 0.2) for the `plugin` world in
[`plugin.wit`](../crates/protocol/wit/plugin.wit), which is the reference:
copy it next to your code. Its package line is the interface's version
(`lsk:plugin@0.7.0`); a `server.wasm` built for another version doesn't
load (its Plugins box says it doesn't fit), so rebuild it against the
current file. New host functions don't change the version; new or
changed exports do.

### How it's called

One call at a time, on a thread of its own; nothing blocks, and the app
does the waiting, so no async is needed in any language.

| Export | Called when |
|---|---|
| `start` | Once, when the plugin starts (the app starts, the plugin is switched on or updated). |
| `run(node, input, values, viewer, run, queue-id, origin)` | One of its effect nodes fired. `viewer` is who set it off, for a node with `"viewer": true` (`none` for others, and for an event without one). `run` is `some(id)` when one of the node's own outputs is wired: pass it to `host.carry-on` to fire one. `queue-id` is `some(id)` when the node waits in the queue and this run's turn came: call `host.done` with it when it has finished. `origin`: `live`, `test` (the panel's Test, "Test this card", a preview) or `replay`. |
| `skip(queue-id)` | The queued run on stage was skipped before it said `done`: undo what stays on. It needn't say `done`; Ended doesn't fire. |
| `settings-changed` | Its plugin-wide settings changed: edited on the Plugins page (not its presets'), a secret saved or forgotten, a file setting's file edited. `host.settings` has the new ones; act again on what came from the old ones (connect again with a new address, say). Not called at `start`. |
| `compute(node, values, run, origin)` → `computed` | One of its value nodes is needed for an event: `values` its inputs and settings, as `run`'s. Answer `now(values)` (its outputs' values by name; one left out gives nothing) or `later` and then `host.compute-later(run, …)`. |
| `on-chat(heard)` | With `"hears": ["chat"]`, allowed: a chat message (`line`: its id, platform, time, `viewer`, `text`, `marks`), or a moderator taking some off (`removed(id)`, `viewer-removed`, `cleared(platform)`). |
| `on-event(heard)` | With `"hears": ["events"]`, allowed: every event (`kind` the built-in event node's id, `bits`, `sub`, `gift_bomb`, `raid`, `reward`, `follow`, `hype_train`, `custom`; its `summary`; the `viewer`; `values` what that node's outputs carry; `origin` `live`, `test` or `replay`). Not chat messages (`on-chat`), nor plugins' events. |
| `nodes-changed(nodes)` | Right after `start`, and whenever the cards change its event nodes (one added or removed, its settings, its card switched on or off): every copy, each with its `id` (for `emit-to`), node, card, whether the card is on, and its settings that aren't filters. |
| `on-tab(json)` | Its tab in the panel sent something (its page's `send`), as JSON. Answer with `host.send-to-tab`. A plugin without a tab is never called. |
| `route(node, input, values, viewer, run, origin)` → `routed` | An event came into one of its flow nodes on `input`. `values` as `run`'s; `viewer` who set it off (`none` for an event without one); `origin` `live`, `test` or `replay`. Answer `go({ output, values })` (one of its event outputs, and its value outputs' values by name), `stop`, or `later` and then `host.route-later(run, …)`. |
| `on-timer(id)` | A timer it set went off. |
| `on-response(id, response)` | A web request it started has an answer, or failed. |
| `on-moderated(id, done)` | The answer to a `host.moderate`: `ok`, or `err(why)`, in Twitch's own words when it refused ("The user specified in the user_id field may not be banned."). |
| `on-socket(id, event)` | A WebSocket it opened: `opened`, a text `message`, a `binary` message, or `closed` with why (also for a message bigger than 4 MB, which closes it). `closed` also comes for a connection that never opened (nothing listening, refused; on Windows only after about 2 s), with no `opened` before it. |
| `speak(request, voice, text, speed)` | A message to make in one of its voices; answer with `host.spoken` and the same `request`. Never called without `voices`. |

What `run` gets as `values` (the WIT `value`): `number`; `text` (`plain`
and `parts`, each a text or an emote); `emotes`; `toggle` (a tick box);
`plain` (a colour, a choice, a position, a preset, a picture's or sound's
path); `empty` (no picture or sound picked).

### What it can call

| `host` function | What it does |
|---|---|
| `log(level, message)` | A line in the app's log, marked with the plugin's id. |
| `emit(node, values)` | Fires one of its event nodes: cards starting with it run. Values by output name: a number, a text (emote codes in it become emotes) or emote codes. A value is converted to its output's type where it can be (a text that reads as a number). |
| `emit-to(placed, output, values)` | Fires one placed copy of an event node (`placed-node.id` from `nodes-changed`), from `output` (`event`, or another of its event outputs): only that card runs. An id or output it hasn't is ignored (the log says so). |
| `emit-output(node, output, values)` | The same, from another of the node's outputs of type `event` ("ended"): the cards wired from that output run. An output the node hasn't is ignored (the log says so). |
| `send-to-page(json)` | Its overlay page gets `{ "type": "message", "data": … }`. Must be JSON. |
| `send-to-tab(json)` | Its tab, in every open panel, gets `{ "type": "message", "data": … }`; a panel not showing it drops it. Must be JSON. |
| `set-tab-count(count)` | The number on its tab ("Requests (2)"), for a `tab` with `"count": true`; 0 shows none. Cleared when it stops or starts again. |
| `set-timer(ms, repeat)` → id, `cancel-timer(id)` | `on-timer` after `ms` (at least 10), and every `ms` if `repeat`. |
| `fetch(request)` → id | A web request (http or https, to a host in `hosts`, not the app itself); the answer comes to `on-response`. Up to 5 redirects to hosts in `hosts` are followed; one elsewhere is the answer. |
| `connect(url)` → id, `send(id, text)`, `send-binary(id, data)`, `close(id)` | A WebSocket (ws or wss, to a host in `hosts`, any port but the app's own on this PC), text and binary messages of at most 4 MB (a bigger one isn't sent; one coming in closes the socket). If it can't be opened, `on-socket` gets `closed` with why. |
| `set-status(level, text)` | A few words on its Plugins box's head, beside its name ("Connected"), with a dot: `ok` (green), `waiting` (grey: nothing wrong, waiting for something) or `problem` (orange, and the header's Plugins dot shows). At most 32 characters (longer is cut); empty takes it away. Cleared when it stops or starts again. |
| `set-note(setting, level, lines)` | A note under one of its settings (the manifest's `settings`, by key) on its Plugins box, or with an empty `setting` on the box itself (under its description), for what the status has no room for: a line each ("“Chat” in “WoW”: …"). `problem` shows it orange, with a warning sign. Empty `lines` takes it away. At most 20 lines of 300 characters (more is cut). Cleared when it stops or starts again. |
| `done(queue-id)` | A queued run has finished: the queue moves on and the node's Ended fires. Only for its own run on stage; one that never says so is given two minutes. |
| `app-port()` | The port the app's overlays and panel are on (5750 unless changed): its pages are `http://localhost:<port>/…`. For links to show the user: server code itself can't reach it. |
| `carry-on(run, output)` | Fires one of an effect node's own outputs for a run (`run`'s id): the card carries on from there. Each output once per run; runs are forgotten after an hour and when the app restarts. |
| `compute-later(run, values)` | The answer to a `compute` that said `later`, within 10 seconds. |
| `route-later(run, output, values)` | The answer to a `route` that said `later`: the event goes on from `output` with `values` on the node's value outputs. Within an hour, not across a restart; each output once per run. |
| `settings()` | The plugin's own settings, every one with a value. Read them when needed: the user can change them while it runs. |
| `set-options(name, options)` | Fills in a list of choices by name, replacing what it had; text settings whose `suggest_from` names it offer them on the panel (an empty list takes it away; while there's none, the field is a plain box). Kept while the app runs, not saved, so fill them in when you start. At most 1000 lists of 1000 choices of 200 bytes. |
| `set-choices(name, choices)` | The same, each choice a `{ value, label }`: the dropdown shows the label, the setting keeps the value (and your code gets it). Use it when what a user sees can change (a button's name) but what it stands for doesn't (an id), so cards keep working after a rename. An empty label shows the value. |
| `spoken(request, audio)` | The answer to `speak`: `ok({ data, format })` with a whole `wav`, `mp3` or `ogg` file, or `err(why)`. |
| `get(key)`, `set(key, value)` | Its own stored values (strings), kept between streams in `plugin_data/<id>.json`. `none` removes a key. |
| `moderate(platform, what)` → id | With `"moderates": true`, allowed: on `platform`'s chat (`twitch` so far), with the streamer's login, times a viewer out (`timeout({ viewer, seconds })`: their id, and 1 second to two weeks), bans one (`ban(id)`), or deletes a message (`delete(id)`: a chat line's `id`, as `on-chat` heard it). The answer comes to `on-moderated`; an error at once says why it can't be done at all (not allowed, not logged in, Moderating off on Status, 20 waiting for Twitch already). Act only on `live` runs (`origin`): a replay brings back an earlier event's real viewer. |

### Limits

Breaking one stops the plugin (see [When it doesn't work](#when-it-doesnt-work)):

- one second per call, `start` included;
- 64 MB of memory;
- 100 timers, 8 WebSockets, 20 moderations waiting for Twitch;
- 4 MB per web answer and per WebSocket message, 30 seconds per web
  request;
- 1 MB of stored values in all;
- no files and no network of its own: web requests and WebSockets only
  through `host`, to its `hosts`, never to the app itself.

### Building it in Rust

Outside this repository:

```toml
# Cargo.toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.62"
```

```rust
// src/lib.rs, with plugin.wit copied to wit/plugin.wit
wit_bindgen::generate!({ path: "wit", world: "plugin" });

use lsk::plugin::host;
use lsk::plugin::types::Level;

struct MyPlugin;

impl Guest for MyPlugin {
    fn start() {
        host::log(Level::Info, "started");
    }
    fn run(
        _node: String,
        _input: String,
        _values: Vec<NamedValue>,
        _viewer: Option<Viewer>,
        _run: Option<u64>,
        _queue_id: Option<u64>,
        _origin: RunOrigin,
    ) {
    }
    fn skip(_queue_id: u64) {}
    fn settings_changed() {}
    fn route(
        _node: String,
        _input: String,
        _values: Vec<NamedValue>,
        _viewer: Option<Viewer>,
        _run: u64,
        _origin: RunOrigin,
    ) -> Routed {
        Routed::Stop
    }
    fn compute(_node: String, _values: Vec<NamedValue>, _run: u64, _origin: RunOrigin) -> Computed {
        Computed::Now(Vec::new())
    }
    fn on_chat(_heard: HeardChat) {}
    fn on_event(_heard: HeardEvent) {}
    fn nodes_changed(_nodes: Vec<PlacedNode>) {}
    fn on_timer(_id: u32) {}
    fn on_response(_id: u32, _response: Result<Response, String>) {}
    fn on_socket(_id: u32, _event: SocketEvent) {}
    fn on_moderated(_id: u32, _done: Result<(), String>) {}
    fn speak(_request: u32, _voice: String, _text: String, _speed: f32) {}
}

export!(MyPlugin);
```

```
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
```

Every export needs a body: the ones above do nothing (a plugin with no
flow or value nodes, that hears nothing, is never asked). Paste them into
a plugin built for an older version when you move it to this one.

Then copy `target/wasm32-wasip2/release/my_plugin.wasm` (dashes in the name
become underscores) to the plugin's folder as `server.wasm`. In this
repository, `cargo plugin <folder>` does both steps.

**Other languages:** any toolchain that makes a WASI 0.2 component for a
WIT world should work (componentize-py, jco, TinyGo, componentize-dotnet),
but only Rust has been tried. Languages that bring a runtime with them may
find the 64 MB and one second (for `start` too) tight.

## Programs

A program connects to the control API's WebSocket and is a plugin while
it's connected: any language, no sandbox, nothing to install in the app.
The connection URL (with the control API's key) is on the panel's Status →
Control API; the WebSocket is at `/control/ws` on the same address.

```
→ { "type": "register", "plugin": { "id": "billybilly", "name": "BillyBilly", "version": "1.0.0", "api": 1, "nodes": [ … ] } }
← { "type": "registered", "plugin": "billybilly" }
→ { "type": "emit", "node": "membership", "values": { "viewer": "Ann", "months": 3, "message": "hi Kappa" } }
← { "type": "emitted", "node": "membership" }
→ { "type": "emit", "node": "membership", "output": "ended", "values": { "viewer": "Ann" } }
← { "type": "emitted", "node": "membership" }
← { "type": "run", "node": "shout", "input": "trigger", "values": { … } }
← { "type": "speak", "request": 7, "voice": "beeps", "text": "Thanks, Ann!", "speed": 1.0 }
→ { "type": "spoken", "request": 7, "audio": "<base64>", "format": "wav" }
← { "type": "route", "run": 12, "node": "regulars", "input": "event", "values": { "at_least": 10 }, "viewer": { … }, "origin": "live" }
→ { "type": "routed", "run": 12, "output": "regular", "values": { "count": 14 } }
← { "type": "compute", "run": 13, "node": "messages", "values": { "viewer": { "text": "Ann", "parts": [ … ] } }, "origin": "live" }
→ { "type": "computed", "run": 13, "values": { "count": 14 } }
← { "type": "nodes", "nodes": [ { "id": "wow/3", "node": "said", "card": "WoW", "on": true, "settings": { "word": { "text": "gg", "parts": [ … ] }, "times": 3 } } ] }
→ { "type": "emit", "to": "wow/3", "values": { "viewer": "Ann" } }
← { "type": "emitted", "node": "said" }
← { "type": "chat", "line": { … } }
← { "type": "heard_event", "kind": "bits", "summary": "Ann cheered 100 bits", "viewer": { … }, "values": { … }, "origin": "live" }
```

- `register` takes a manifest as `plugin.json` would be, once per
  connection. A program has no folder, so its `icon` is the SVG itself
  (`"<svg …>…</svg>"`), not a file name. A program can't fill in choices
  or set notes (`set-options`, `set-choices`, `set-note` are server
  code's only). It's refused (an `error`, status 400) when the manifest
  has a problem, a built-in or installed plugin has its id, or another
  connected program already is that plugin.
- `emit` values are as server code's `emit`: a number, a text, or a list of
  emote codes. Values that don't fit an output's type are converted where
  they can be (a text that reads as a number), else left out; `summary`
  defaults to the node's title. The event isn't in the history, and a
  per-viewer cooldown works as a plain one for it (it has no viewer id).
  With `output`, it fires that output of type `event` instead
  of `event` (as server code's `emit-output`); one the node hasn't is
  refused (an `error`, status 404).
- `run` is the message a page gets, with the same [values](#values).
- `speak` is answered within 30 seconds with `spoken`: `audio` (base64)
  and `format` (`wav`, `mp3`, `ogg`), or `error` with why.
- `route` comes when an event reaches one of its [flow nodes](#nodes), as
  server code's `route`: `values` as a run's, `viewer` who set it off (left
  out for an event without one), `origin` `live`, `test` or `replay`.
  Answer `routed` with the same `run`: `output` (one of the node's event
  outputs) and `values` by its value outputs' names, or no `output` to
  send the event nowhere. The event waits for it, up to an hour.
- `compute` comes when one of its [value nodes](#nodes) is needed for an
  event, `values` its inputs and settings. Answer `computed` with the same
  `run` and its outputs' values by name (one left out gives nothing),
  within 10 seconds; after that the node gives nothing.
- What `routed` and `computed` give is as a run's [values](#values): a
  number, `true` or `false`, a plain string, a text (`{ "text", "parts" }`,
  its emotes kept), or emotes. Each is converted to its output's type
  where it can be (a text that reads as a number). A good answer gets no
  reply; one for a run that doesn't wait (answered already, or too late)
  is refused (an `error`, status 404), as is an output the node hasn't
  (the event then goes nowhere).
- `nodes` comes right after `registered` (for a plugin with event nodes)
  and whenever the cards change them (a copy added or removed, its
  settings, its card switched on or off): every copy, each with its `id`,
  node, card, whether the card is on, and its settings that aren't
  filters, as a run's values (a `many` list as a text with a line each).
  `emit` with `to` and one of those ids fires only that copy (`node` may
  then be left out); an id it hasn't is refused (404).
- With `"hears"` allowed on the Plugins page: the chat as `chat`,
  `chat_removed`, `chat_user_removed` and `chat_cleared`, the messages a
  page gets ([Chat messages](#chat-messages)); every event as
  `heard_event`, as server code's `on-event` (`kind`, `summary`,
  `viewer`, `values` as a run's, `origin`). They come one at a time: when
  the program doesn't keep up, 500 wait at most, then the oldest are
  dropped.
- When the program goes, events waiting at its flow nodes go nowhere and
  its value nodes give nothing, at once.
- A program has no overlay page and no queued nodes: its effects are
  whatever the program does.
- Its node types are remembered, so cards keep its nodes while it isn't
  running. Its event nodes then don't fire, and its effects go nowhere.
- The connection gets the control API's state messages too, and can send
  any other control API request ([control-api.md](control-api.md)).
  Every plugin's switch, a program's too, is in its `plugins`, by the
  plugin's id: a Stream Deck key can switch it.

## Examples to start from

| Where | Shows |
|---|---|
| [`examples/plugins/confetti`](../examples/plugins/confetti) | The smallest page: a manifest, a canvas, `run`. |
| [`examples/plugins/bits-to-money`](../examples/plugins/bits-to-money) | Rust server code with only a value node (`compute`): a cheer's bits at a rate, as a number and as text. |
| [`examples/plugins/copypasta`](../examples/plugins/copypasta) | Rust server code that hears the chat (`on-chat`) and fires one placed event node at a time (`nodes-changed`, `emit-to`), each with its own settings. |
| [`examples/plugins/counter`](../examples/plugins/counter) | Rust server code: an effect node, an event node, a value node (Current count: `compute`) and a flow node (Every other: `route`), `emit`, stored values, `send-to-page`, and a tab in the panel (`on-tab`, `send-to-tab`) to set the counts right. |
| [`examples/programs/fake-platform.mjs`](../examples/programs/fake-platform.mjs) | A program (Node, nothing to install): an event node, an effect node, a voice. |
| [`examples/programs/chat-regulars.mjs`](../examples/programs/chat-regulars.mjs) | A program (Node) that hears the chat and events: a value node (`compute`), a flow node (`route`), and an event node fired one copy at a time (`nodes`, `emit` with `to`), each with its own settings. |
| [`crates/e2e/plugins/host-check`](../crates/e2e/plugins/host-check) | Rust server code using every `host` call: timers, web requests, WebSockets, settings (and `settings-changed`), a secret, choices, a voice, an effect node's own output (Relay), a queued node (Hold: `done`, `skip`), a flow node answering later (Later: `route-later`), a tab with a number on it (`set-tab-count`). |
| [`crates/moderation`](../crates/moderation) with [its manifest](../crates/server/plugins/moderation/plugin.json) | Rust server code that moderates (`moderates`, `host.moderate`, `on-moderated`), with effect nodes that get the viewer (`viewer`) and leave tests alone (`origin`), and a note on its box when it didn't work. |
| [`crates/obs`](../crates/obs) with [its manifest](../crates/server/plugins/obs/plugin.json) | A whole plugin in Rust server code: a WebSocket kept open, a secret setting, choices filled in and following each other (`sources/{scene}`), a status, a note under a setting, `settings-changed` (connects again on a new address), own outputs, queued runs with `skip`, event nodes with a filter (`many`). |
| [`crates/server/plugins/sound`](../crates/server/plugins/sound) | The queue (`done`, `skip`, a run sent twice), the master volume, a required sound. Plain JavaScript. |
| [`crates/server/plugins/media`](../crates/server/plugins/media) | A media setting: pictures, and videos with their sound; an optional queue. |
| [`crates/server/plugins/speak`](../crates/server/plugins/speak) | A text input read aloud (`"speak": true`), playing `speech`. |
| [`crates/server/plugins/particles`](../crates/server/plugins/particles) | An emotes input, a preview, settings shown by another's value (`is`), a picture folder as a list (`many`). |
| [`crates/alert`](../crates/alert) with [its manifest](../crates/server/plugins/alert/plugin.json) | A page in Rust: presets, `changes`, text styles, `page_hosts`, speech in the queue. |
| [`crates/chat`](../crates/chat) with [its manifest](../crates/server/plugins/chat/plugin.json) | A page in Rust: `hears`, `keep`, a `file` setting, `help`, `own_source`, a preview with samples, `viewer`. |
| [`crates/rain`](../crates/rain) with [its manifest](../crates/server/plugins/rain/plugin.json) | A page in Rust: an emotes setting with a count, `when`, sliders in groups, animated emotes. |

## Not possible yet

- A list (`many`) anywhere but an event node's text settings.
- Effect nodes' own outputs fired by a page or a program (only server
  code fires them).
- Server code's effect runs don't bring `changes` or preset `settings`.
