// The screens of the PWA: the pairing screen, and the chat with the agent on the laptop.

import { Connection, type State } from "./connection.ts";
import { parsePairingLink } from "./pairing.ts";
import type { ToPhone } from "./protocol.ts";
import { forgetLaptop, identity, loadLaptop, saveLaptop, type Laptop } from "./store.ts";
import { Timeline, type Item } from "./timeline.ts";
import "./style.css";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const pairScreen = $("pair");
const pairForm = $<HTMLFormElement>("pair-form");
const pairLink = $<HTMLInputElement>("pair-link");
const pairError = $("pair-error");
const chatScreen = $("chat");
const nameView = $("name");
const stateView = $("state");
const timelineView = $("timeline");
const composer = $<HTMLFormElement>("composer");
const prompt = $<HTMLTextAreaElement>("prompt");
const sendButton = $<HTMLButtonElement>("send");
const stopButton = $<HTMLButtonElement>("stop");
const unpairButton = $<HTMLButtonElement>("unpair");

const timeline = new Timeline();
let connection: Connection | null = null;
let state: State = { kind: "connecting" };
let status: { agent: string; busy: boolean; queued: number } | null = null;

function showPairing(error = "") {
  connection?.close();
  connection = null;
  chatScreen.hidden = true;
  pairScreen.hidden = false;
  pairError.textContent = error;
}

// Read a pairing link and connect with its one-time code. The laptop is saved only after the
// laptop accepts the phone.
function pairWith(url: string) {
  try {
    const { link, key } = parsePairingLink(url);
    void connect({ relay: link.relay, laptop: link.laptop, key, name: "Laptop" }, link.code);
  } catch (error) {
    showPairing(`This is not a good pairing link: ${(error as Error).message}.`);
  }
}

async function connect(laptop: Laptop, pair?: string) {
  connection?.close();
  timeline.clear();
  status = null;
  pairScreen.hidden = true;
  chatScreen.hidden = false;
  nameView.textContent = laptop.name;
  render();
  const { device, key } = await identity();
  const current: Connection = new Connection({
    relay: laptop.relay,
    laptop: laptop.laptop,
    laptopKey: laptop.key,
    device,
    key,
    pair,
    onState(next) {
      if (connection !== current) return;
      state = next;
      if (next.kind === "stopped" && next.unpaired) {
        void forgetLaptop();
        return showPairing(next.reason);
      }
      render();
    },
    onReady(name) {
      nameView.textContent = name;
      void saveLaptop({ ...laptop, name });
      current.send({ type: "sync", after: timeline.seq });
    },
    onMessage: receive,
  });
  connection = current;
}

function receive(message: ToPhone) {
  switch (message.type) {
    case "status":
      status = message;
      break;
    case "event":
      timeline.apply(message.seq, message.event);
      break;
    case "reset":
      timeline.clear();
      timeline.seq = message.first - 1;
      break;
    case "error":
      timeline.items.push({ kind: "error", message: message.message });
      break;
    case "accepted":
      return;
  }
  render();
}

let frame = 0;
function render() {
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    const ready = state.kind === "ready";
    stateView.textContent = stateText();
    stateView.dataset.state = ready && status?.busy ? "busy" : state.kind;
    sendButton.disabled = !ready;
    stopButton.hidden = !(ready && status?.busy);

    const atBottom = timelineView.scrollHeight - timelineView.scrollTop - timelineView.clientHeight < 40;
    timelineView.replaceChildren(...timeline.items.map(itemView));
    if (timeline.items.length === 0) {
      timelineView.append(h("p", { class: "empty" }, ready ? "Send a message to the agent on your laptop." : ""));
    }
    if (atBottom) timelineView.scrollTop = timelineView.scrollHeight;
  });
}

function stateText(): string {
  switch (state.kind) {
    case "connecting":
      return "Connecting";
    case "offline":
      return "Laptop offline";
    case "stopped":
      return state.reason;
    case "ready": {
      if (!status) return "Connected";
      const queued = status.queued ? `, ${status.queued} queued` : "";
      return `${status.agent}: ${status.busy ? "working" : "ready"}${queued}`;
    }
  }
}

function itemView(item: Item): HTMLElement {
  switch (item.kind) {
    case "user":
      return h("div", { class: "user" }, item.text);
    case "agent":
      return h("div", { class: "agent" }, item.text);
    case "thought":
      return h("details", { class: "thought" }, h("summary", {}, "Thought"), h("p", {}, item.text));
    case "tool": {
      const tool = h("details", { class: `tool ${item.status}${item.parent ? " child" : ""}` });
      tool.append(h("summary", {}, h("span", { class: "dot" }), item.title));
      if (item.detail) tool.append(h("pre", {}, item.detail));
      return tool;
    }
    case "plan":
      return h("ol", { class: "plan" }, ...item.entries.map((entry) => h("li", { class: entry.status }, entry.text)));
    case "permission": {
      const card = h("div", { class: "permission" }, h("p", {}, item.title));
      if (item.answer) card.append(h("p", { class: "answer" }, item.answer));
      else if (!item.open) card.append(h("p", { class: "answer" }, "Answered on the laptop"));
      else {
        const buttons = item.options.map((option) => {
          const button = h("button", { class: option.allow ? "allow" : "deny" }, option.name);
          button.onclick = () => {
            if (!connection?.send({ type: "permission", request: item.request, option: option.id })) return;
            item.answer = option.name;
            render();
          };
          return button;
        });
        card.append(h("div", { class: "options" }, ...buttons));
      }
      return card;
    }
    case "end":
      return h("p", { class: "end" }, item.reason === "end_turn" ? "Done" : item.reason);
    case "error":
      return h("p", { class: "error" }, item.message);
  }
}

function h(tag: string, attributes: Record<string, string>, ...children: (Node | string)[]): HTMLElement {
  const element = document.createElement(tag);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
  element.append(...children);
  return element;
}

pairForm.onsubmit = (event) => {
  event.preventDefault();
  pairWith(pairLink.value.trim());
};

composer.onsubmit = (event) => {
  event.preventDefault();
  const text = prompt.value.trim();
  if (!text) return;
  if (connection?.send({ type: "prompt", id: crypto.randomUUID(), text })) prompt.value = "";
};

prompt.onkeydown = (event) => {
  if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) composer.requestSubmit();
};

stopButton.onclick = () => connection?.send({ type: "cancel" });

unpairButton.onclick = async () => {
  if (!confirm("Unpair this phone from the laptop?")) return;
  await forgetLaptop();
  showPairing();
};

// A phone stops the WebSocket of a page in the background. Connect again when the page shows.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") connection?.wake();
});
addEventListener("online", () => connection?.wake());

if (import.meta.env.PROD && "serviceWorker" in navigator) void navigator.serviceWorker.register("/sw.js");

if (location.hash.startsWith("#pair=")) {
  const url = location.href;
  // The code is good for one pairing only. Remove it from the address and the history.
  history.replaceState(null, "", location.pathname);
  pairWith(url);
} else {
  const laptop = await loadLaptop();
  if (laptop) void connect(laptop);
  else showPairing();
}
