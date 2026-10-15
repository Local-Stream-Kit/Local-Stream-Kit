// Hype train bar: a bar that fills up as a hype train goes, with its level
// and the time it has left, looking however the streamer's CSS (the `look`
// file setting) says; help.html has the markup and its classes. Ships with
// Local Stream Kit as a plugin like any other: see docs/writing-a-plugin.md.
// Messages from the overlay:
//
//   { type: "settings", values: { look: "…css…", position: "top",
//     linger: 8 } }
//   { type: "run", node: "bar", issued_at_ms, values: { title: { text,
//     parts }, class: { text: "golden_kappa" }, level: 2, progress: 640,
//     goal: 1000, seconds_left: 185 } }
//     (from a Hype train node: Seconds left is 0 once the train has ended)
//   { type: "kept", value }    (what this page kept: a train still going
//                               when the overlay reloaded)

"use strict";

// Position's nine names, as how far across and down (0–1); anywhere
// else comes as { x, y } in %.
const PLACES = {
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

function place(position) {
  if (typeof position === "string") return PLACES[position] || PLACES.top;
  const part = (value) => (Number.isFinite(value) ? Math.min(1, Math.max(0, value / 100)) : null);
  const [x, y] = [part(position?.x), part(position?.y)];
  return x === null || y === null ? PLACES.top : [x, y];
}

// Which edge it's nearest, for looks that slide in from that side.
function edge(y) {
  return y < 1 / 3 ? "at-top" : y > 2 / 3 ? "at-bottom" : "at-middle";
}
const EDGES = ["at-top", "at-middle", "at-bottom"];
// A train that runs out without an end coming (a missed message) ends
// this long after its time is up.
const GRACE_MS = 3000;
// How long `.level-up` stays on after a new level, for its animation.
const LEVEL_UP_MS = 2500;

const element = (id) => document.getElementById(id);
const stage = element("stage");
const box = element("train");

let settings = { linger: 8 };
// The train shown: { title, classes, level, progress, goal, ends_at_ms,
// ended_at_ms }, or null.
let train = null;
let hideTimer = null;
let levelUpTimer = null;
// The classes the node gave the bar, to take off again.
let nodeClasses = [];

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "settings") {
    settings = message.values || {};
    applySettings();
  } else if (message.type === "run" && message.node === "bar") {
    run(message.values || {}, message.issued_at_ms);
  } else if (message.type === "kept") {
    restore(message.value);
  }
});

function applySettings() {
  element("look").textContent = settings.look || "";
  const [x, y] = place(settings.position);
  stage.style.setProperty("--columns", `${x}fr auto ${1 - x}fr`);
  stage.style.setProperty("--rows", `${y}fr auto ${1 - y}fr`);
  box.classList.remove(...EDGES);
  box.classList.add(edge(y));
}
applySettings();

function number(value) {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}

function text(value) {
  return typeof value === "string" ? value : value?.text || "";
}

function run(values, issuedAtMs) {
  const now = issuedAtMs || Date.now();
  const secondsLeft = number(values.seconds_left);
  const level = Math.round(number(values.level)) || 1;
  const previous = train;
  train = {
    title: values.title ?? "Hype train",
    classes: text(values.class).split(/\s+/).filter(Boolean),
    level,
    progress: number(values.progress),
    goal: number(values.goal),
    ends_at_ms: now + secondsLeft * 1000,
    ended_at_ms: secondsLeft > 0 ? null : now,
  };
  window.parent.postMessage({ type: "keep", value: train }, "*");
  show(previous && level > previous.level && train.ended_at_ms === null);
}

// A train still going (or just ended) when the overlay reloaded.
function restore(kept) {
  if (!kept || train) return;
  const end = kept.ended_at_ms ?? kept.ends_at_ms + GRACE_MS;
  if (Date.now() > end + lingerMs()) return;
  train = kept;
  box.classList.add("restored");
  show(false);
}

function lingerMs() {
  return Math.max(0, settings.linger ?? 8) * 1000;
}

function show(levelUp) {
  clearTimeout(hideTimer);
  hideTimer = null;
  title(train.title);
  box.classList.remove(...nodeClasses);
  nodeClasses = train.classes || [];
  box.classList.add(...nodeClasses);
  element("level").textContent = `Level ${train.level}`;
  const ended = train.ended_at_ms !== null;
  const share = ended ? 1 : train.goal > 0 ? Math.min(1, train.progress / train.goal) : 0;
  box.style.setProperty("--progress", share.toFixed(3));
  element("fill").style.width = `${(share * 100).toFixed(1)}%`;
  element("points").textContent = ended || train.goal <= 0 ? "" : `${points(train.progress)} / ${points(train.goal)}`;
  box.classList.toggle("ended", ended);
  if (levelUp) {
    box.classList.remove("level-up");
    void box.offsetWidth; // starts its animation again
    box.classList.add("level-up");
    clearTimeout(levelUpTimer);
    levelUpTimer = setTimeout(() => box.classList.remove("level-up"), LEVEL_UP_MS);
  }
  box.classList.add("shown");
  tick();
}

function points(value) {
  return Math.round(value).toLocaleString();
}

// The title, with its emotes as pictures and the parts a card's text
// marked with a text style, as the chat display has them.
function title(value) {
  const target = element("title");
  target.replaceChildren();
  const parts = typeof value === "string" ? [{ type: "text", text: value }] : value?.parts;
  for (const part of parts || [{ type: "text", text: text(value) }]) {
    let node;
    if (part.type === "emote" && part.url) {
      node = document.createElement("span");
      node.className = "emote";
      [part.url, ...(part.overlays || [])].forEach((url, index) => {
        const image = document.createElement("img");
        image.className = index === 0 ? "emote__image" : "emote__overlay";
        image.src = url;
        image.alt = index === 0 ? part.code || "" : "";
        node.append(image);
      });
    } else {
      node = document.createElement("span");
      node.textContent = part.text ?? part.code ?? "";
    }
    const style = part.style;
    if (style) {
      node.classList.add("text-style", `text-style-${style.name}`);
      if (style.font) node.style.fontFamily = style.font;
      if (style.size_percent) node.style.fontSize = `${style.size_percent}%`;
      if (style.bold !== null && style.bold !== undefined) node.style.fontWeight = style.bold ? "800" : "400";
      if (style.italic !== null && style.italic !== undefined) node.style.fontStyle = style.italic ? "italic" : "normal";
      if (style.colour) node.style.color = style.colour;
    }
    target.append(node);
  }
}

function tick() {
  if (!train) return;
  const now = Date.now();
  // Ran out without an end: ends now.
  if (train.ended_at_ms === null && now > train.ends_at_ms + GRACE_MS) {
    train.ended_at_ms = train.ends_at_ms;
    show(false);
    return;
  }
  if (train.ended_at_ms !== null) {
    element("time").textContent = "Ended";
    if (hideTimer === null) {
      hideTimer = setTimeout(hide, Math.max(0, train.ended_at_ms + lingerMs() - now));
    }
    return;
  }
  const left = Math.max(0, Math.ceil((train.ends_at_ms - now) / 1000));
  element("time").textContent = `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}
setInterval(tick, 250);

function hide() {
  hideTimer = null;
  train = null;
  box.classList.remove("shown", "level-up", "ended", "restored");
  window.parent.postMessage({ type: "keep", value: null }, "*");
}
