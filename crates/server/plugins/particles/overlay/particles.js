// Particles: confetti, hearts, stars, snow, pictures or emotes, flying
// across the overlay. Ships with Local Stream Kit as a plugin like any
// other: see docs/writing-a-plugin.md. Messages from the overlay:
//
//   { type: "settings", values: { most_pieces: 3000 } }
//   { type: "run", node: "burst", values: { shape: "confetti",
//     from: "corners", colour: "#ff4f8b", rainbow: true,
//     size: 14, seconds: 5, count: 150,
//     picture: ["/pictures/a.png"], flip_x: false, flip_y: false,
//     emotes: [{ code, url, overlays }] } }
//
// The picture and the flips come only with the shape "picture" (a picture
// folder: all its pictures, one at random for each piece), the emotes
// only when wired in: the pieces are then those emotes, taken in turn.
// Pictures and emotes are drawn 2.5 times the size. From "fireworks":
// pops at random places over the screen, spread over the time it lasts.
//
// Many bursts at once (a big gift of subs, or Show on overlay pressed
// over and over) stay smooth: each piece is drawn with one transform and
// one fill (no saving and restoring the canvas's state), the shapes are
// made once, and past the Most pieces on screen setting the oldest fade
// out quickly.

"use strict";

const canvas = document.getElementById("particles");
const context = canvas.getContext("2d");
const GRAVITY = 1400; // px/s²
const AIR = 0.35; // of the sideways speed kept each second
const IMAGE_SCALE = 2.5;
const RAINBOW = ["#ff595e", "#ffca3a", "#8ac926", "#1982c4", "#6a4c93", "#ff924c", "#f15bb5"];
// Fireworks: a pop's pieces slow down fast in every direction, then
// drift down.
const POP_GRAVITY = 300;
const POP_AIR = 0.12;
const POP_PIECES = 30;
const MOST_POPS = 25;
// A piece fades out over its last FADE seconds. Past `mostPieces` on
// screen the oldest fade out over QUICK_FADE instead, and past twice as
// many they go at once. A picture or emote counts as IMAGE_COST pieces:
// drawing one takes about as long as that many shapes.
const FADE = 0.8;
const QUICK_FADE = 0.4;
const IMAGE_COST = 4;

let mostPieces = 3000; // the setting, until it comes
let pieces = [];
let running = false;
let last = 0;
let ratio = 1; // device pixels per CSS pixel
const images = new Map(); // url → <img>
const glows = new Map(); // colour → a pop's flash in it

// The canvas matches the page, checked every frame: in an iframe (as the
// overlay and the control panel's preview host this page) the page's size
// can arrive after this script has run, with no resize event.
function fit() {
  ratio = window.devicePixelRatio || 1;
  const width = Math.round(innerWidth * ratio);
  const height = Math.round(innerHeight * ratio);
  if (canvas.width === width && canvas.height === height) return;
  canvas.width = width;
  canvas.height = height;
}

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message) return;
  if (message.type === "settings") {
    const most = Number((message.values || {}).most_pieces);
    if (most > 0) mostPieces = most;
    // Lowered: what's flying past it goes now.
    makeRoom(0);
  } else if (message.type === "run" && message.node === "burst") {
    burst(message.values || {});
  }
});

function random(min, max) {
  return min + Math.random() * (max - min);
}

function pickOne(list) {
  return list[Math.floor(Math.random() * list.length)];
}

function image(url) {
  if (!images.has(url)) {
    const img = new Image();
    img.src = url;
    images.set(url, img);
  }
  return images.get(url);
}

// The shapes, made once, 1 from the middle to the edge: drawn scaled.
const STAR = new Path2D();
for (let i = 0; i < 10; i++) {
  const radius = i % 2 === 0 ? 1 : 0.45;
  const angle = (i * Math.PI) / 5 - Math.PI / 2;
  STAR.lineTo(Math.cos(angle) * radius, Math.sin(angle) * radius);
}
STAR.closePath();
const HEART = new Path2D();
HEART.moveTo(0, 0.35);
HEART.bezierCurveTo(-1.1, -0.35, -0.45, -1.05, 0, -0.45);
HEART.bezierCurveTo(0.45, -1.05, 1.1, -0.35, 0, 0.35);
const DOT = new Path2D();
DOT.arc(0, 0, 1, 0, Math.PI * 2);
// A shape's path, and its size for a piece of size 1 (confetti has none:
// it's a rectangle).
const SHAPES = { hearts: [HEART, 1], stars: [STAR, 1], snow: [DOT, 0.35] };

/// A pop's flash in `colour`: a glow, white in the middle, 1 across from
/// the middle to the edge (drawn scaled, as the shapes are).
function glow(colour) {
  let found = glows.get(colour);
  if (!found) {
    found = context.createRadialGradient(0, 0, 0, 0, 0, 1);
    found.addColorStop(0, "rgba(255, 255, 255, 0.9)");
    found.addColorStop(0.3, colour);
    found.addColorStop(1, colour + "00");
    glows.set(colour, found);
  }
  return found;
}

/// What the pieces look like: a list of images each (an emote and the
/// zero-width ones on it, or a picture), or a shape.
function looks(values) {
  const emotes = Array.isArray(values.emotes) ? values.emotes : [];
  if (emotes.length > 0) {
    return emotes.map((emote) => ({ images: [emote.url, ...(emote.overlays || [])].map(image) }));
  }
  if (values.shape === "picture") {
    // A list; a single path too, as it was before folders.
    const pictures = [values.picture].flat().filter((url) => typeof url === "string" && url);
    return pictures.length > 0 ? pictures.map((url) => ({ images: [image(url)] })) : [{ shape: "confetti" }];
  }
  return [{ shape: values.shape || "confetti" }];
}

/// Every piece has the same fields, so the browser keeps them quick.
function makePiece(fields) {
  return Object.assign(
    {
      images: null, shape: "", path: null, scale: 1, colour: "", x: 0, y: 0, vx: 0, vy: 0, gravity: 0,
      air: 1, allWays: false, fall: 0, sway: 0, size: 0, ratio: 1, rotation: 0, spin: 0, upright: false,
      flip: 0, flipSpeed: 0, mirrorX: 1, mirrorY: 1, wait: 0, age: 0, life: 0, fade: FADE, cost: 1,
    },
    fields,
  );
}

/// Where a piece starts and how fast it goes, for `from`.
function launch(from, i, w, h) {
  // Fast enough to get about two thirds of the way up.
  const up = Math.sqrt(2 * GRAVITY * h * 0.7);
  if (from === "top") {
    // Spread out over the first moments, so they don't fall as a line.
    return { x: random(0, w), y: random(-h * 0.6, -20), vx: random(-100, 100), vy: random(0, 150) };
  }
  if (from === "middle") {
    const angle = random(0, Math.PI * 2);
    const speed = random(300, 1200);
    return { x: w / 2, y: h / 2, vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed - 400 };
  }
  if (from === "bottom") {
    return { x: random(0, w), y: h + 10, vx: random(-200, 200), vy: -up * random(0.6, 1.1) };
  }
  // Half from each bottom corner, up and in.
  const left = i % 2 === 0;
  return {
    x: left ? 0 : w,
    y: h,
    vx: (left ? 1 : -1) * random(300, 1300) * (w / 1920),
    vy: -up * random(0.8, 1.2),
  };
}

/// Fireworks: where and when each pop goes off, and its colour. The first
/// goes off at once; the rest over the time it lasts, less a pop's own.
function pops(count, seconds, w, h, colour, rainbow) {
  const many = Math.max(1, Math.min(MOST_POPS, Math.round(count / POP_PIECES)));
  const scale = Math.min(w, h) / 1080;
  return Array.from({ length: many }, (_, i) => ({
    x: random(w * 0.15, w * 0.85),
    y: random(h * 0.15, h * 0.55),
    wait: i === 0 ? 0 : random(0, Math.max(0, seconds - 2)),
    speed: random(420, 620) * scale,
    colour: rainbow ? pickOne(RAINBOW) : colour,
  }));
}

/// Room for pieces costing `coming`: past `mostPieces` the oldest (they're
/// in the order they came) fade out quickly, past twice as many they go.
function makeRoom(coming) {
  let total = coming;
  for (const piece of pieces) total += piece.cost;
  let gone = 0;
  while (gone < pieces.length && total > 2 * mostPieces) total -= pieces[gone++].cost;
  pieces.splice(0, gone);
  for (let i = 0; i < pieces.length && total > mostPieces; i++) {
    const old = pieces[i];
    total -= old.cost;
    const left = old.life - old.age;
    // Already going (a pop's flash always is).
    if (left <= QUICK_FADE) continue;
    // From where its fade is now, down to nothing.
    const alpha = Math.min(1, left / old.fade);
    old.life = old.age + QUICK_FADE;
    old.fade = QUICK_FADE / alpha;
  }
}

function burst(values) {
  const count = Math.max(1, Math.min(1000, Math.round(values.count ?? 150)));
  const seconds = Math.max(1, values.seconds ?? 5);
  const size = Math.max(4, values.size ?? 14);
  const colour = values.colour || "#ff4f8b";
  const rainbow = values.rainbow !== false;
  const from = values.from || "corners";
  const kinds = looks(values);
  // Emotes in turn; pictures from a folder at random.
  const inTurn = Array.isArray(values.emotes) && values.emotes.length > 0;
  const snow = kinds[0].shape === "snow";
  const w = innerWidth;
  const h = innerHeight;
  const fireworks = from === "fireworks" ? pops(count, seconds, w, h, colour, rainbow) : null;
  const made = [];
  for (const pop of fireworks || []) {
    made.push(makePiece({ shape: "flash", path: DOT, colour: pop.colour, x: pop.x, y: pop.y,
      size: pop.speed * 0.35, wait: pop.wait, life: 0.35 }));
  }
  for (let i = 0; i < count; i++) {
    const look = inTurn ? kinds[i % kinds.length] : pickOne(kinds);
    const [path, scale] = SHAPES[look.shape] || [null, 1];
    const pop = fireworks && fireworks[i % fireworks.length];
    let start;
    if (pop) {
      const angle = random(0, Math.PI * 2);
      const speed = pop.speed * random(0.5, 1);
      start = { x: pop.x, y: pop.y, vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed };
    } else {
      start = launch(from, i, w, h);
    }
    const pieceSize = look.images ? size * IMAGE_SCALE * random(0.8, 1.2) : size * random(0.6, 1.1);
    made.push(makePiece({
      images: look.images || null,
      shape: look.shape || "",
      path,
      scale: scale * pieceSize,
      ...start,
      gravity: pop ? POP_GRAVITY : GRAVITY,
      air: pop ? POP_AIR : AIR,
      // Pops slow down upwards and downwards too.
      allWays: !!pop,
      // How fast it drifts down once it's falling.
      fall: snow ? random(40, 110) : pop ? random(60, 140) : random(110, 240),
      sway: snow ? random(20, 60) : 25,
      size: pieceSize,
      ratio: random(0.4, 0.9),
      rotation: random(0, Math.PI * 2),
      spin: look.images || look.shape === "hearts" ? random(-3, 3) : random(-10, 10),
      // Pictures, emotes and hearts stay upright, with a wobble.
      upright: !!look.images || look.shape === "hearts",
      flip: random(0, Math.PI * 2),
      flipSpeed: random(2, 8),
      mirrorX: values.flip_x && Math.random() < 0.5 ? -1 : 1,
      mirrorY: values.flip_y && Math.random() < 0.5 ? -1 : 1,
      colour: pop ? pop.colour : rainbow ? pickOne(RAINBOW) : colour,
      wait: pop ? pop.wait : 0,
      life: pop ? random(1.4, 2.2) : seconds * random(0.8, 1.1) + (from === "top" ? 2 : 0),
      cost: look.images ? IMAGE_COST * look.images.length : 1,
    }));
  }
  makeRoom(made.reduce((total, piece) => total + piece.cost, 0));
  for (const piece of made) pieces.push(piece);
  if (!running) {
    running = true;
    last = performance.now();
    requestAnimationFrame(frame);
  }
}

/// A piece's next position; false once it's gone.
function move(piece, dt, h) {
  piece.age += dt;
  const kept = Math.pow(piece.air, dt);
  piece.vx *= kept;
  if (piece.allWays) piece.vy *= kept;
  piece.vy = Math.min(piece.vy + piece.gravity * dt, piece.fall);
  piece.flip += piece.flipSpeed * dt;
  piece.x += (piece.vx + Math.sin(piece.flip) * piece.sway) * dt;
  piece.y += piece.vy * dt;
  piece.rotation += piece.spin * dt;
  return piece.age <= piece.life && piece.y <= h + 2 * piece.size + 50;
}

/// One piece: a transform and a fill. Pieces off the screen (from the
/// top, they start above it) aren't drawn.
function draw(piece, w, h) {
  let alpha = Math.min(1, (piece.life - piece.age) / piece.fade);
  let scale = piece.scale;
  if (piece.shape === "flash") {
    // Grows as it fades.
    const done = piece.age / piece.life;
    alpha = 1 - done;
    scale = Math.max(1, piece.size * (0.4 + done));
  }
  // Wide enough for all of it: an emote can be several times as wide as
  // it's tall.
  const reach = (piece.images ? 4 : 2) * piece.size;
  if (alpha <= 0 || piece.x < -reach || piece.x > w + reach || piece.y < -reach || piece.y > h + reach) return;
  const angle = piece.upright ? Math.sin(piece.rotation) * 0.3 : piece.rotation;
  const cos = Math.cos(angle) * ratio;
  const sin = Math.sin(angle) * ratio;
  const x = piece.x * ratio;
  const y = piece.y * ratio;
  context.globalAlpha = alpha;
  if (piece.images) {
    const size = piece.size;
    context.setTransform(cos * piece.mirrorX, sin * piece.mirrorX, -sin * piece.mirrorY, cos * piece.mirrorY, x, y);
    for (const img of piece.images) {
      if (!img.complete || img.naturalHeight === 0) continue;
      const width = (size * img.naturalWidth) / img.naturalHeight;
      context.drawImage(img, -width / 2, -size / 2, width, size);
    }
  } else if (piece.path) {
    context.setTransform(cos * scale, sin * scale, -sin * scale, cos * scale, x, y);
    context.fillStyle = piece.shape === "flash" ? glow(piece.colour) : piece.colour;
    context.fill(piece.path);
  } else {
    // Confetti: turning over, so narrower as it flips.
    const squash = Math.abs(Math.cos(piece.flip)) * 0.8 + 0.2;
    const size = piece.size;
    context.setTransform(cos, sin, -sin * squash, cos * squash, x, y);
    context.fillStyle = piece.colour;
    context.fillRect(-size / 2, (-size * piece.ratio) / 2, size, size * piece.ratio);
  }
}

function frame(now) {
  // A frame's time can be a little before `last` was taken.
  const dt = Math.max(0, Math.min(0.05, (now - last) / 1000));
  last = now;
  fit();
  const w = innerWidth;
  const h = innerHeight;
  context.setTransform(1, 0, 0, 1, 0, 0);
  context.clearRect(0, 0, canvas.width, canvas.height);
  // The ones still going, kept in place.
  let kept = 0;
  for (let i = 0; i < pieces.length; i++) {
    const piece = pieces[i];
    // A firework's pieces wait for their pop.
    if (piece.wait > 0) {
      piece.wait -= dt;
      pieces[kept++] = piece;
    } else if (move(piece, dt, h)) {
      pieces[kept++] = piece;
      draw(piece, w, h);
    }
  }
  pieces.length = kept;
  if (kept > 0) {
    requestAnimationFrame(frame);
  } else {
    running = false;
  }
}
