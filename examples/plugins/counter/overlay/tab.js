// The Counter plugin's tab in the panel: every counter with its count, −
// and + to set one right, and Add counter for a new one at 0. It asks the
// server code for the counts when it opens (an empty message), and gets
// them again after every change, made here, in another panel or by a card:
//
//   { type: "message", data: { counts: [{ counter: "Deaths", count: 12 }] } }

"use strict";

const list = document.getElementById("counters");
const addButton = document.getElementById("add-counter");
const addForm = document.getElementById("add-form");
const addName = document.getElementById("add-name");

/** To the server code (its `on-tab`). */
function send(data) {
  window.parent.postMessage({ type: "send", data }, "*");
}

function button(text, label, by, counter) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "small step";
  button.textContent = text;
  button.setAttribute("aria-label", `${label} ${counter}`);
  button.addEventListener("click", () => send({ counter, by }));
  return button;
}

function show(counts) {
  if (counts.length === 0) return;
  list.replaceChildren(
    ...counts.map(({ counter, count }) => {
      const row = document.createElement("div");
      row.className = "row";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = counter;
      const value = document.createElement("span");
      value.className = "count";
      value.textContent = String(count);
      row.append(name, button("−", "One less", -1, counter), value, button("+", "One more", 1, counter));
      return row;
    }),
  );
}

/** Add counter opens the name field; adding, Escape, or leaving it empty
 * closes it again (focus back on Add counter, unless it went elsewhere). */
function adding(open, focus = true) {
  addButton.hidden = open;
  addForm.hidden = !open;
  addName.value = "";
  if (focus) (open ? addName : addButton).focus();
}

function add() {
  const name = addName.value.trim();
  if (name) send({ add: name });
  adding(false);
}

addButton.addEventListener("click", () => adding(true));
document.getElementById("add-confirm").addEventListener("click", add);
addName.addEventListener("keydown", (event) => {
  if (event.key === "Enter") add();
  else if (event.key === "Escape") adding(false);
});
addForm.addEventListener("focusout", (event) => {
  if (!addForm.contains(event.relatedTarget) && !addName.value.trim()) adding(false, false);
});

addEventListener("message", (event) => {
  const message = event.data;
  if (event.source !== window.parent || !message || message.type !== "message") return;
  if (Array.isArray(message.data?.counts)) show(message.data.counts);
});

send({});
