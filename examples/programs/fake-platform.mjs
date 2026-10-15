// A program that is a Local Stream Kit plugin from outside: it connects to
// the control API's WebSocket, says which nodes it brings, sends events
// into its event node, is told when its effect node fires, and reads
// messages aloud in its voice (a beep a word). See
// docs/writing-a-plugin.md → Programs.
//
// Run it with the connection URL from the panel's Status → Control API page:
//
//   node examples/programs/fake-platform.mjs "http://localhost:5750/control/?key=…"
//
// Then add "Fake membership" and "Print" nodes to a card, and pick the
// voice "Beeps" (under Fake platform) on Plugins → Speak. Press
// Enter here to send a membership; Ctrl+C to stop. Needs Node 22 or later (its
// built-in WebSocket); nothing to install.

import readline from "node:readline";

const given = process.argv[2];
if (!given) {
  console.error('Give it the connection URL: node fake-platform.mjs "http://localhost:5750/control/?key=…"');
  process.exit(1);
}
const url = new URL(given);
url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
url.pathname = "/control/ws";

const manifest = {
  id: "fake-platform",
  name: "Fake platform",
  version: "0.1.0",
  api: 1,
  author: "Local Stream Kit",
  description: "Memberships from a platform that doesn't exist, a node that prints, and a voice that beeps.",
  voices: [{ id: "beeps", name: "Beeps" }],
  nodes: [
    {
      id: "membership",
      title: "Fake membership",
      category: "events",
      outputs: [
        { name: "event", label: "Event", type: "event" },
        { name: "viewer", label: "Viewer", type: "text" },
        { name: "months", label: "Months", type: "number" },
        { name: "message", label: "Message", type: "text" },
        { name: "summary", label: "Summary", type: "text" },
      ],
    },
    {
      id: "print",
      title: "Print",
      category: "effects",
      inputs: [
        { name: "trigger", label: "Trigger", type: "event" },
        { name: "text", label: "Text", type: "text" },
      ],
      settings: [
        { key: "text", label: "Text", type: "text", default: "{viewer}: {message}" },
        { key: "shout", label: "In capitals", type: "bool", default: false },
      ],
    },
  ],
};

const viewers = ["Ann", "Bo", "Cy", "Dee"];
let sent = 0;

const socket = new WebSocket(url);
socket.addEventListener("open", () => {
  socket.send(JSON.stringify({ type: "register", plugin: manifest }));
});
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  switch (message.type) {
    case "registered":
      console.log(`Connected as the ${message.plugin} plugin. Press Enter to send a membership.`);
      break;
    case "run": {
      // One of our effect nodes fired: texts come as { text, parts }.
      const text = message.values.text?.text ?? "";
      console.log(`Print: ${message.values.shout ? text.toUpperCase() : text}`);
      break;
    }
    case "speak": {
      // A message to read in one of our voices: answer with its audio.
      const audio = beeps(message.text, message.speed).toString("base64");
      socket.send(JSON.stringify({ type: "spoken", request: message.request, audio, format: "wav" }));
      console.log(`Said in beeps: ${message.text}`);
      break;
    }
    case "error":
      console.error(`The app says: ${message.error} (${message.status})`);
      break;
    // "state" (cards, effects, the queue) and "emitted" aren't needed here.
  }
});
socket.addEventListener("close", () => {
  console.log("Disconnected.");
  process.exit(0);
});
socket.addEventListener("error", () => {
  console.error(`Couldn't connect to ${url.origin}. Is Local Stream Kit running, and is the key right?`);
});

/** A beep a word (longer words higher), as a WAV file: 16 kHz, mono, 16 bits. */
function beeps(text, speed) {
  const rate = 16000;
  const samples = [];
  for (const word of text.split(/\s+/).filter(Boolean)) {
    const pitch = 300 + Math.min(word.length, 12) * 60;
    const tone = Math.round((rate * 0.12) / speed);
    for (let i = 0; i < tone; i++) {
      const fade = Math.min(1, i / 200, (tone - i) / 200);
      samples.push(Math.round(Math.sin((2 * Math.PI * pitch * i) / rate) * 8000 * fade));
    }
    for (let i = 0; i < Math.round((rate * 0.08) / speed); i++) samples.push(0);
  }
  const wav = Buffer.alloc(44 + samples.length * 2);
  wav.write("RIFF", 0);
  wav.writeUInt32LE(36 + samples.length * 2, 4);
  wav.write("WAVEfmt ", 8);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20); // PCM
  wav.writeUInt16LE(1, 22); // mono
  wav.writeUInt32LE(rate, 24);
  wav.writeUInt32LE(rate * 2, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write("data", 36);
  wav.writeUInt32LE(samples.length * 2, 40);
  samples.forEach((sample, i) => wav.writeInt16LE(sample, 44 + i * 2));
  return wav;
}

readline.createInterface({ input: process.stdin }).on("line", () => {
  const viewer = viewers[sent++ % viewers.length];
  const months = 1 + Math.floor(Math.random() * 24);
  socket.send(
    JSON.stringify({
      type: "emit",
      node: "membership",
      values: { viewer, months, message: "Hello from the fake platform Kappa", summary: `${viewer} joined for ${months} months` },
    }),
  );
  console.log(`Sent: ${viewer}, ${months} months`);
});
