// The Counter plugin's overlay page: shows "Hugs: 12" in the top right
// corner for a few seconds whenever the plugin's server code sends a count:
//
//   { type: "message", data: { counter: "Hugs", count: "12" } }

"use strict";

const box = document.getElementById("count");
let hide = 0;

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message || message.type !== "message") return;
  const data = message.data || {};
  box.textContent = `${data.counter}: ${data.count}`;
  box.classList.add("shown");
  clearTimeout(hide);
  hide = setTimeout(() => box.classList.remove("shown"), 4000);
});
