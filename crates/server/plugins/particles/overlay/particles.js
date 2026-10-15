// Particles: confetti, hearts, stars, snow, a picture or emotes, flying
// across the overlay. Ships with Local Stream Kit as a plugin like any
// other: see docs/writing-a-plugin.md. A message from the overlay:
//
//   { type: "run", node: "burst", values: { shape: "confetti",
//     picture: null, from: "corners", colour: "#ff4f8b", rainbow: true,
//     size: 14, seconds: 5, count: 150,
//     emotes: [{ code, url, overlays }] } }   (emotes only when wired in)
//
// With emotes wired in, the pieces are those emotes, taken in turn.
// Pictures and emotes are drawn 2.5 times the size.

"use strict";

const canvas = document.getElementById("particles");
const context = canvas.getContext("2d");
const GRAVITY = 1400; // px/s²
const AIR = 0.35; // of the sideways speed kept each second
const IMAGE_SCALE = 2.5;
const RAINBOW = ["#ff595e", "#ffca3a", "#8ac926", "#1982c4", "#6a4c93", "#ff924c", "#f15bb5"];

let pieces = [];
let running = false;
let last = 0;
const images = new Map(); // url → <img>

function resize() {
  const ratio = window.devicePixelRatio || 1;
  canvas.width = Math.round(innerWidth * ratio);
  canvas.height = Math.round(innerHeight * ratio);
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
}
addEventListener("resize", resize);
resize();

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message || message.type !== "run") return;
  if (message.node === "burst") burst(message.values || {});
});

function random(min, max) {
  return min + Math.random() * (max - min);
}

function image(url) {
  if (!images.has(url)) {
    const img = new Image();
    img.src = url;
    images.set(url, img);
  }
  return images.get(url);
}

/// What each piece looks like: a list of images (an emote and the
/// zero-width ones on it), or a shape.
function looks(values) {
  const emotes = Array.isArray(values.emotes) ? values.emotes : [];
  if (emotes.length > 0) {
    return emotes.map((emote) => ({ images: [emote.url, ...(emote.overlays || [])].map(image) }));
  }
  if (values.shape === "picture") {
    return values.picture ? [{ images: [image(values.picture)] }] : [{ shape: "confetti" }];
  }
  return [{ shape: values.shape || "confetti" }];
}

function burst(values) {
  const count = Math.max(1, Math.min(1000, Math.round(values.count ?? 150)));
  const seconds = Math.max(1, values.seconds ?? 5);
  const size = Math.max(4, values.size ?? 14);
  const colour = values.colour || "#ff4f8b";
  const from = values.from || "corners";
  const kinds = looks(values);
  const snow = kinds[0].shape === "snow";
  const w = innerWidth;
  const h = innerHeight;
  // Fast enough to get about two thirds of the way up.
  const up = Math.sqrt(2 * GRAVITY * h * 0.7);
  for (let i = 0; i < count; i++) {
    let x, y, vx, vy;
    if (from === "top") {
      x = random(0, w);
      // Spread out over the first moments, so they don't fall as a line.
      y = random(-h * 0.6, -20);
      vx = random(-100, 100);
      vy = random(0, 150);
    } else if (from === "middle") {
      const angle = random(0, Math.PI * 2);
      const speed = random(300, 1200);
      x = w / 2;
      y = h / 2;
      vx = Math.cos(angle) * speed;
      vy = Math.sin(angle) * speed - 400;
    } else if (from === "bottom") {
      x = random(0, w);
      y = h + 10;
      vx = random(-200, 200);
      vy = -up * random(0.6, 1.1);
    } else {
      // Half from each bottom corner, up and in.
      const left = i % 2 === 0;
      x = left ? 0 : w;
      y = h;
      vx = (left ? 1 : -1) * random(300, 1300) * (w / 1920);
      vy = -up * random(0.8, 1.2);
    }
    const look = kinds[i % kinds.length];
    pieces.push({
      ...look,
      x,
      y,
      vx,
      vy,
      // How fast it drifts down once it's falling.
      fall: snow ? random(40, 110) : random(110, 240),
      sway: snow ? random(20, 60) : 25,
      size: look.images ? size * IMAGE_SCALE * random(0.8, 1.2) : size * random(0.6, 1.1),
      ratio: random(0.4, 0.9),
      rotation: random(0, Math.PI * 2),
      spin: look.images || look.shape === "hearts" ? random(-3, 3) : random(-10, 10),
      flip: random(0, Math.PI * 2),
      flipSpeed: random(2, 8),
      colour: values.rainbow === false ? colour : RAINBOW[Math.floor(Math.random() * RAINBOW.length)],
      age: 0,
      life: seconds * random(0.8, 1.1) + (from === "top" ? 2 : 0),
    });
  }
  if (!running) {
    running = true;
    last = performance.now();
    requestAnimationFrame(frame);
  }
}

function star(r) {
  context.beginPath();
  for (let i = 0; i < 10; i++) {
    const radius = i % 2 === 0 ? r : r * 0.45;
    const angle = (i * Math.PI) / 5 - Math.PI / 2;
    context.lineTo(Math.cos(angle) * radius, Math.sin(angle) * radius);
  }
  context.closePath();
  context.fill();
}

function heart(r) {
  context.beginPath();
  context.moveTo(0, r * 0.35);
  context.bezierCurveTo(-r * 1.1, -r * 0.35, -r * 0.45, -r * 1.05, 0, -r * 0.45);
  context.bezierCurveTo(r * 0.45, -r * 1.05, r * 1.1, -r * 0.35, 0, r * 0.35);
  context.fill();
}

function draw(piece) {
  if (piece.images) {
    // Upright-ish: a small wobble instead of tumbling.
    for (const img of piece.images) {
      if (!img.complete || img.naturalHeight === 0) continue;
      const width = (piece.size * img.naturalWidth) / img.naturalHeight;
      context.drawImage(img, -width / 2, -piece.size / 2, width, piece.size);
    }
    return;
  }
  context.fillStyle = piece.colour;
  switch (piece.shape) {
    case "hearts":
      heart(piece.size);
      break;
    case "stars":
      star(piece.size);
      break;
    case "snow":
      context.fillStyle = piece.colour;
      context.beginPath();
      context.arc(0, 0, piece.size * 0.35, 0, Math.PI * 2);
      context.fill();
      break;
    default:
      // Confetti: turning over, so narrower as it flips.
      context.scale(1, Math.abs(Math.cos(piece.flip)) * 0.8 + 0.2);
      context.fillRect(-piece.size / 2, (-piece.size * piece.ratio) / 2, piece.size, piece.size * piece.ratio);
  }
}

function frame(now) {
  // A frame's time can be a little before `last` was taken.
  const dt = Math.max(0, Math.min(0.05, (now - last) / 1000));
  last = now;
  const kept = Math.pow(AIR, dt);
  context.clearRect(0, 0, innerWidth, innerHeight);
  pieces = pieces.filter((piece) => {
    piece.age += dt;
    piece.vx *= kept;
    piece.vy = Math.min(piece.vy + GRAVITY * dt, piece.fall);
    piece.flip += piece.flipSpeed * dt;
    piece.x += (piece.vx + Math.sin(piece.flip) * piece.sway) * dt;
    piece.y += piece.vy * dt;
    piece.rotation += piece.spin * dt;
    if (piece.age > piece.life || piece.y > innerHeight + 2 * piece.size + 50) return false;
    context.save();
    // Fades out over its last 0.8 s.
    context.globalAlpha = Math.max(0, Math.min(1, (piece.life - piece.age) / 0.8));
    context.translate(piece.x, piece.y);
    // Pictures, emotes and hearts stay upright, with a wobble.
    const upright = piece.images || piece.shape === "hearts";
    context.rotate(upright ? Math.sin(piece.rotation) * 0.3 : piece.rotation);
    draw(piece);
    context.restore();
    return true;
  });
  if (pieces.length > 0) {
    requestAnimationFrame(frame);
  } else {
    running = false;
    context.clearRect(0, 0, innerWidth, innerHeight);
  }
}
