// Speak: plays the spoken message the app made of the node's Text (text to
// speech), right away or in its turn in the queue. Ships with Local Stream
// Kit as a plugin like any other: see docs/writing-a-plugin.md. Messages
// from the overlay:
//
//   { type: "run", node: "speak", values: { text: { text: "Thanks, Ann!",
//     parts: [ … ], speech: { url: "/speech/….wav", duration_ms: 1800,
//     volume: 0.8 } }, queue: true } }
//     (speech is missing when nothing is read: text to speech off or not
//     installed, or an empty text; with queue_id when it's its turn in the
//     queue: once it has been read, this page answers { type: "done",
//     queue_id }; with try: true for the Speak box's Try, not queued: a
//     new one stops the one before)
//   { type: "skip", queue_id: 12 }                  (the queue skipped it)
//   { type: "volume", volume: 0.8, muted: false }   (the master volume)

"use strict";

// A message whose audio doesn't say it ended is over this long after its
// length (a minute if its length isn't known).
const SLACK_MS = 2000;
const UNKNOWN_MS = 60000;
// After a message, before the queue moves on.
const GAP_MS = 400;

let master = { volume: 1, muted: false };
const playing = new Set(); // <audio>s, with their own volume (0–1)
const queued = new Map(); // queue id → what stops it
let stopTry = null; // what stops the Speak box's Try being read

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "volume") {
    master = { volume: message.volume, muted: message.muted };
    for (const audio of playing) applyVolume(audio);
  } else if (message.type === "run" && message.node === "speak") {
    speak(message.values || {}, message.queue_id);
  } else if (message.type === "skip") {
    queued.get(message.queue_id)?.();
  }
});

function applyVolume(audio) {
  audio.volume = Math.max(0, Math.min(1, audio.ownVolume * master.volume));
  audio.muted = master.muted;
}

// A queued message has been read: the queue moves on.
function done(queueId) {
  if (queueId === undefined) return;
  queued.delete(queueId);
  window.parent.postMessage({ type: "done", queue_id: queueId }, "*");
}

function speak(values, queueId) {
  // Sent again after the overlay reconnected: it's already being read.
  if (queueId !== undefined && queued.has(queueId)) return;
  const speech = values.text?.speech;
  if (!speech?.url) {
    done(queueId);
    return;
  }
  const audio = new Audio(speech.url);
  audio.ownVolume = Math.max(0, Math.min(1, speech.volume ?? 1));
  applyVolume(audio);
  playing.add(audio);

  let over = false;
  const end = (gap) => {
    if (over) return;
    over = true;
    clearTimeout(timer);
    audio.pause();
    playing.delete(audio);
    setTimeout(() => done(queueId), gap);
  };
  const timer = setTimeout(() => end(0), (speech.duration_ms ?? UNKNOWN_MS) + SLACK_MS);
  if (queueId !== undefined) {
    // Skipped: it stops, and the queue has already moved on.
    queued.set(queueId, () => {
      queued.delete(queueId);
      queueId = undefined;
      end(0);
    });
  }
  if (values.try) {
    stopTry?.();
    stopTry = () => end(0);
  }
  audio.addEventListener("ended", () => end(GAP_MS));
  audio.addEventListener("error", () => end(0));
  // A page nobody has clicked in may not play sound (OBS always may).
  audio.play().catch(() => end(0));
}
