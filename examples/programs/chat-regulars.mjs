// A program that is a Local Stream Kit plugin with nodes that answer. It
// hears the chat (and events, which it only prints), counts each viewer's
// messages since it started, and brings:
//
// - Messages (a value node): how many messages a viewer has sent;
// - Regulars first (a flow node): an event goes on from Regular when its
//   viewer has sent at least that many messages, else from New;
// - Word said (an event node): fires its card when its Word has been said
//   that many Times in the chat; each copy on a card counts for itself.
//
// See docs/writing-a-plugin.md → Programs. Run it with the connection URL
// from the panel's Status → Control API page:
//
//   node examples/programs/chat-regulars.mjs "http://localhost:5750/control/?key=…"
//
// Then press Allow on Plugins → Chat regulars (it hears the chat and
// events only once allowed), and add its nodes to a card. Ctrl+C to stop.
// Needs Node 22 or later (its built-in WebSocket); nothing to install.

const given = process.argv[2];
if (!given) {
  console.error('Give it the connection URL: node chat-regulars.mjs "http://localhost:5750/control/?key=…"');
  process.exit(1);
}
const url = new URL(given);
url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
url.pathname = "/control/ws";

const manifest = {
  id: "chat-regulars",
  name: "Chat regulars",
  version: "0.1.0",
  api: 1,
  author: "Local Stream Kit",
  description: "Counts each viewer's chat messages: who's a regular, and words said often.",
  hears: ["chat", "events"],
  nodes: [
    {
      id: "messages",
      title: "Messages",
      category: "values",
      inputs: [{ name: "viewer", label: "Viewer", type: "text" }],
      outputs: [{ name: "count", label: "Messages", type: "number" }],
      settings: [{ key: "viewer", label: "Viewer", type: "text", placeholder: "Their name" }],
    },
    {
      id: "regulars",
      title: "Regulars first",
      category: "flow",
      inputs: [{ name: "event", label: "Event", type: "event" }],
      outputs: [
        { name: "regular", label: "Regular", type: "event" },
        { name: "new", label: "New", type: "event" },
        { name: "count", label: "Messages", type: "number" },
      ],
      settings: [{ key: "at_least", label: "Messages", type: "number", default: 10, min: 1 }],
    },
    {
      id: "said",
      title: "Word said",
      category: "events",
      outputs: [
        { name: "event", label: "Event", type: "event" },
        { name: "viewer", label: "Viewer", type: "text" },
        { name: "summary", label: "Summary", type: "text" },
      ],
      settings: [
        { key: "word", label: "Word", type: "text", default: "gg" },
        { key: "times", label: "Times", type: "number", default: 3, min: 1 },
      ],
    },
  ],
};

/** Messages by viewer (their login name). */
const counts = new Map();
/** Our Word said nodes on the cards, each with its own settings. */
let placed = [];
/** How often each one's word was said since it last fired, by its id. */
const said = new Map();

const socket = new WebSocket(url);
const send = (message) => socket.send(JSON.stringify(message));

socket.addEventListener("open", () => send({ type: "register", plugin: manifest }));
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  switch (message.type) {
    case "registered":
      console.log(`Connected as the ${message.plugin} plugin.`);
      break;
    case "nodes":
      placed = message.nodes;
      console.log(`Word said is on ${placed.length} card(s).`);
      break;
    case "chat":
      heardChat(message.line);
      break;
    case "heard_event":
      console.log(`Heard: ${message.summary}`);
      break;
    case "compute": {
      // Messages: texts come as { text, parts }.
      const name = (message.values.viewer?.text ?? "").trim().toLowerCase();
      send({ type: "computed", run: message.run, values: { count: counts.get(name) ?? 0 } });
      break;
    }
    case "route": {
      // Regulars first: the event waits for this answer (the card doesn't).
      const count = counts.get(message.viewer?.name) ?? 0;
      const output = count >= message.values.at_least ? "regular" : "new";
      send({ type: "routed", run: message.run, output, values: { count } });
      break;
    }
    case "error":
      console.error(`The app says: ${message.error} (${message.status})`);
      break;
    // "state", and moderators taking messages off (chat_removed…), aren't
    // needed here.
  }
});
socket.addEventListener("close", () => {
  console.log("Disconnected.");
  process.exit(0);
});
socket.addEventListener("error", () => {
  console.error(`Couldn't connect to ${url.origin}. Is Local Stream Kit running, and is the key right?`);
});

/** A chat message: counted, and each Word said whose word is in it. */
function heardChat(line) {
  if (!line.user) return;
  counts.set(line.user.name, (counts.get(line.user.name) ?? 0) + 1);
  const words = line.parts
    .map((part) => part.text ?? part.code)
    .join("")
    .toLowerCase()
    .split(/\s+/);
  for (const node of placed.filter((node) => node.on)) {
    const word = node.settings.word.text.trim().toLowerCase();
    if (!word || !words.includes(word)) continue;
    const times = (said.get(node.id) ?? 0) + 1;
    if (times < node.settings.times) {
      said.set(node.id, times);
      continue;
    }
    // Only this copy's card runs.
    said.set(node.id, 0);
    const summary = `"${word}" was said ${times} times`;
    send({ type: "emit", to: node.id, values: { viewer: line.user.display_name, summary } });
    console.log(`${summary}, on ${node.card}`);
  }
}
