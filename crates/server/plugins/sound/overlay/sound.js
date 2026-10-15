// Sounds: plays a sound from the sounds folder, right away (sounds may
// overlap) or in its turn in the queue. Ships with Local Stream Kit as a
// plugin like any other: see docs/writing-a-plugin.md. Messages from the
// overlay:
//
//   { type: "run", node: "play", values: { sound: "/sounds/horn.mp3",
//     volume: 80, queue: false } }
//     (with queue_id when it's its turn in the queue: once the sound has
//     ended, this page answers { type: "done", queue_id })
//   { type: "skip", queue_id: 12 }                  (the queue skipped it)
//   { type: "volume", volume: 0.8, muted: false }   (the master volume)

"use strict";

// In case a sound never says it ended (the queue gives up after two
// minutes anyway).
const MAX_SECONDS = 600;

let master = { volume: 1, muted: false };
const playing = new Set(); // <audio>s, with their own volume (0–1)
const queued = new Map(); // queue id → what stops it

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "volume") {
    master = { volume: message.volume, muted: message.muted };
    for (const audio of playing) applyVolume(audio);
  } else if (message.type === "run" && message.node === "play") {
    play(message.values || {}, message.queue_id);
  } else if (message.type === "skip") {
    queued.get(message.queue_id)?.();
  }
});

// Muted, it plays on silently: unmuted mid-sound, the rest is heard.
function applyVolume(audio) {
  audio.volume = Math.max(0, Math.min(1, audio.ownVolume * master.volume));
  audio.muted = master.muted;
}

// A queued sound has ended: the queue moves on.
function done(queueId) {
  if (queueId === undefined) return;
  queued.delete(queueId);
  window.parent.postMessage({ type: "done", queue_id: queueId }, "*");
}

function play(values, queueId) {
  // Sent again after the overlay reconnected: it's already playing.
  if (queueId !== undefined && queued.has(queueId)) return;
  if (!values.sound) {
    done(queueId);
    return;
  }
  const audio = new Audio(values.sound);
  audio.ownVolume = Math.max(0, Math.min(1, (values.volume ?? 80) / 100));
  applyVolume(audio);
  playing.add(audio);

  let over = false;
  const end = () => {
    if (over) return;
    over = true;
    clearTimeout(timer);
    audio.pause();
    playing.delete(audio);
    done(queueId);
  };
  const timer = setTimeout(end, MAX_SECONDS * 1000);
  if (queueId !== undefined) {
    // Skipped: it stops, and the queue has already moved on.
    queued.set(queueId, () => {
      queued.delete(queueId);
      queueId = undefined;
      end();
    });
  }
  audio.addEventListener("ended", end);
  audio.addEventListener("error", end);
  // A page nobody has clicked in may not play sound (OBS always may).
  audio.play().catch(end);
}
