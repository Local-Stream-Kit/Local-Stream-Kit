// The confetti plugin's overlay page. Local Stream Kit loads it in an iframe
// on the overlay and passes it a message whenever a card's Confetti node
// fires (see docs/writing-a-plugin.md → Overlay pages):
//
//   { type: "run", node: "burst", input: "trigger", issued_at_ms: …,
//     values: { pieces: 150, from: "corners", colour: "#ffcc00",
//               rainbow: true, seconds: 5 } }

"use strict";

const canvas = document.getElementById("confetti");
const context = canvas.getContext("2d");
const GRAVITY = 1400; // px/s²
const AIR = 0.35; // of the sideways speed kept each second
const RAINBOW = ["#ff595e", "#ffca3a", "#8ac926", "#1982c4", "#6a4c93", "#ff924c", "#f15bb5"];

let pieces = [];
let running = false;
let last = 0;

function resize() {
  const ratio = window.devicePixelRatio || 1;
  canvas.width = Math.round(innerWidth * ratio);
  canvas.height = Math.round(innerHeight * ratio);
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
}
addEventListener("resize", resize);
resize();

// Only messages from the overlay itself: other plugins' pages could send
// some too.
addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message || message.type !== "run") return;
  if (message.node === "burst") burst(message.values || {});
});

function random(min, max) {
  return min + Math.random() * (max - min);
}

function burst(values) {
  const count = Math.max(1, Math.min(1000, Math.round(values.pieces ?? 150)));
  const seconds = Math.max(1, values.seconds ?? 5);
  const colour = values.colour || "#ffcc00";
  const from = values.from || "corners";
  const w = innerWidth;
  const h = innerHeight;
  // Fast enough to get about two thirds of the way up.
  const up = Math.sqrt(2 * GRAVITY * h * 0.7);
  for (let i = 0; i < count; i++) {
    let x, y, vx, vy;
    if (from === "top") {
      x = random(0, w);
      y = random(-60, -10);
      vx = random(-150, 150);
      vy = random(0, 200);
    } else if (from === "middle") {
      const angle = random(0, Math.PI * 2);
      const speed = random(300, 1200);
      x = w / 2;
      y = h / 2;
      vx = Math.cos(angle) * speed;
      vy = Math.sin(angle) * speed - 400;
    } else {
      // Half from each bottom corner, up and in.
      const left = i % 2 === 0;
      x = left ? 0 : w;
      y = h;
      vx = (left ? 1 : -1) * random(300, 1300) * (w / 1920);
      vy = -up * random(0.8, 1.2);
    }
    pieces.push({
      x,
      y,
      vx,
      vy,
      // How fast it flutters down once it's falling.
      fall: random(110, 240),
      size: random(8, 15),
      ratio: random(0.4, 0.9),
      rotation: random(0, Math.PI * 2),
      spin: random(-10, 10),
      flip: random(0, Math.PI * 2),
      flipSpeed: random(4, 11),
      round: Math.random() < 0.2,
      colour: values.rainbow === false ? colour : RAINBOW[Math.floor(Math.random() * RAINBOW.length)],
      age: 0,
      life: seconds * random(0.8, 1.1),
    });
  }
  if (!running) {
    running = true;
    last = performance.now();
    requestAnimationFrame(frame);
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
    piece.x += (piece.vx + Math.sin(piece.flip) * 25) * dt;
    piece.y += piece.vy * dt;
    piece.rotation += piece.spin * dt;
    if (piece.age > piece.life || piece.y > innerHeight + 50) return false;
    context.save();
    // Fades out over its last 0.8 s.
    context.globalAlpha = Math.min(1, (piece.life - piece.age) / 0.8);
    context.translate(piece.x, piece.y);
    context.rotate(piece.rotation);
    // Turning over: narrower as it flips.
    context.scale(1, Math.abs(Math.cos(piece.flip)) * 0.8 + 0.2);
    context.fillStyle = piece.colour;
    if (piece.round) {
      context.beginPath();
      context.arc(0, 0, piece.size * 0.4, 0, Math.PI * 2);
      context.fill();
    } else {
      context.fillRect(-piece.size / 2, (-piece.size * piece.ratio) / 2, piece.size, piece.size * piece.ratio);
    }
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
