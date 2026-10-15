// Show media: a picture or a video on the overlay, right away (several can
// be up at once) or in its turn in the queue. Ships with Local Stream Kit
// as a plugin like any other: see docs/writing-a-plugin.md. Messages
// from the overlay:
//
//   { type: "run", node: "show", values: { media: "/pictures/clip.webm",
//     position: "center", height: 400, animation: "pop", tilt: 0,
//     volume: 80, seconds: 0, queue: false } }
//     (with queue_id when it's its turn in the queue: once it's gone, this
//     page answers { type: "done", queue_id })
//   { type: "skip", queue_id: 12 }                  (the queue skipped it)
//   { type: "volume", volume: 0.8, muted: false }   (the master volume)

"use strict";

const VIDEO = /\.(webm|mp4)$/i;
const MARGIN = 40; // px from the edges
const PICTURE_SECONDS = 5; // a picture's length when it's 0
const MAX_VIDEO_SECONDS = 600; // in case a video never says it ended
// Position's nine names, as how far across and down (0–1); anywhere else
// comes as { x, y } in %, and "random" is anywhere.
const POSITIONS = {
  top_left: [0, 0],
  top: [0.5, 0],
  top_right: [1, 0],
  left: [0, 0.5],
  center: [0.5, 0.5],
  right: [1, 0.5],
  bottom_left: [0, 1],
  bottom: [0.5, 1],
  bottom_right: [1, 1],
};

function anchor(position) {
  if (typeof position === "string") return POSITIONS[position] || [Math.random(), Math.random()];
  const part = (value) => (Number.isFinite(value) ? Math.min(1, Math.max(0, value / 100)) : null);
  const [x, y] = [part(position?.x), part(position?.y)];
  return x === null || y === null ? [Math.random(), Math.random()] : [x, y];
}

let master = { volume: 1, muted: false };
const playing = new Set(); // videos, with their own volume (0–1)
const queued = new Map(); // queue id → what removes it

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "volume") {
    master = { volume: message.volume, muted: message.muted };
    for (const video of playing) applyVolume(video);
  } else if (message.type === "run" && message.node === "show") {
    show(message.values || {}, message.queue_id);
  } else if (message.type === "skip") {
    queued.get(message.queue_id)?.();
  }
});

// A queued item has ended: the queue moves on.
function done(queueId) {
  if (queueId === undefined || !queued.has(queueId)) return;
  queued.delete(queueId);
  window.parent.postMessage({ type: "done", queue_id: queueId }, "*");
}

function applyVolume(video) {
  video.volume = Math.max(0, Math.min(1, video.ownVolume * master.volume));
  video.muted = master.muted || video.ownVolume === 0;
}

function show(values, queueId) {
  // Sent again after the overlay reconnected: it's already up.
  if (queued.has(queueId)) return;
  if (!values.media) {
    if (queueId !== undefined) window.parent.postMessage({ type: "done", queue_id: queueId }, "*");
    return;
  }
  const isVideo = VIDEO.test(values.media);
  const element = document.createElement(isVideo ? "video" : "img");
  element.className = "item";
  element.src = values.media;
  const height = Math.min(values.height || 400, innerHeight);
  element.style.height = `${height}px`;
  element.style.setProperty("--tilt", `${values.tilt || 0}deg`);
  const animation = values.animation || "pop";

  // Once its size is known, it can be placed so it stays on screen.
  const place = () => {
    const width = height * (element.naturalWidth || element.videoWidth || 1) / (element.naturalHeight || element.videoHeight || 1);
    const [ax, ay] = anchor(values.position);
    const left = MARGIN + width / 2;
    const top = MARGIN + height / 2;
    const x = left + ax * Math.max(0, innerWidth - 2 * left);
    const y = top + ay * Math.max(0, innerHeight - 2 * top);
    element.style.left = `${x}px`;
    element.style.top = `${y}px`;
    if (animation !== "none") element.classList.add(`in-${animation}`);
    document.body.append(element);
  };

  let ended = false;
  const remove = () => {
    if (ended) return;
    ended = true;
    playing.delete(element);
    if (animation === "none") {
      element.remove();
      done(queueId);
      return;
    }
    element.classList.remove(`in-${animation}`);
    element.classList.add(`out-${animation}`);
    const gone = () => {
      element.remove();
      done(queueId);
    };
    element.addEventListener("animationend", gone, { once: true });
    setTimeout(gone, 1000);
  };
  if (queueId !== undefined) queued.set(queueId, remove);
  const seconds = values.seconds > 0 ? values.seconds : null;

  element.addEventListener("error", remove);
  if (isVideo) {
    element.playsInline = true;
    // Given a length, a shorter video plays again until it's up.
    element.loop = seconds !== null;
    element.ownVolume = Math.max(0, Math.min(1, (values.volume ?? 80) / 100));
    applyVolume(element);
    playing.add(element);
    element.addEventListener("loadedmetadata", () => {
      place();
      // A page nobody has clicked in may not play sound (OBS always may):
      // then it plays without.
      element.play().catch(() => {
        element.muted = true;
        element.play().catch(remove);
      });
    }, { once: true });
    element.addEventListener("ended", remove);
    setTimeout(remove, (seconds ?? MAX_VIDEO_SECONDS) * 1000);
  } else {
    element.decode().then(place, remove);
    setTimeout(remove, (seconds ?? PICTURE_SECONDS) * 1000);
  }
}
