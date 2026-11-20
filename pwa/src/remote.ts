// The state of the phone app: the pairing, the connection to the laptop, and the timeline. React
// reads it through `useRemote`. The state changes outside of React, when messages come from the
// laptop, so this is an external store.

import { useSyncExternalStore } from "react";
import { Connection, type State } from "./connection.ts";
import { parsePairingLink } from "./pairing.ts";
import type { PermissionOption, ToLaptop, ToPhone } from "./protocol.ts";
import { forgetLaptop, identity, loadLaptop, saveLaptop, type Laptop } from "./store.ts";
import { Timeline, type Item } from "./timeline.ts";

export type Screen = "loading" | "pairing" | "chat";
export type AgentStatus = { agent: string; busy: boolean; queued: number };
export type PermissionItem = Extract<Item, { kind: "permission" }>;

class Remote {
  screen: Screen = "loading";
  pairError = "";
  name = "";
  state: State = { kind: "connecting" };
  status: AgentStatus | null = null;
  timeline = new Timeline();
  // React renders again when this number changes.
  version = 0;

  #connection: Connection | null = null;
  #listeners = new Set<() => void>();
  #frame = 0;

  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  // Tell React about a change. The laptop can send many events in one frame, so React renders
  // once for each frame.
  #changed() {
    if (this.#frame) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      this.version++;
      for (const listener of this.#listeners) listener();
    });
  }

  async start() {
    if (location.hash.startsWith("#pair=")) {
      const url = location.href;
      // The code is good for one pairing only. Remove it from the address and the history.
      history.replaceState(null, "", location.pathname);
      return this.pair(url);
    }
    const laptop = await loadLaptop();
    if (laptop) void this.#connect(laptop);
    else this.#showPairing();
  }

  // Read a pairing link and connect with its one-time code. The laptop is saved only after the
  // laptop accepts the phone.
  pair(url: string) {
    try {
      const { link, key } = parsePairingLink(url);
      void this.#connect({ relay: link.relay, laptop: link.laptop, key, name: "Laptop" }, link.code);
    } catch (error) {
      this.#showPairing(`This is not a good pairing link: ${(error as Error).message}.`);
    }
  }

  async unpair() {
    await forgetLaptop();
    this.#showPairing();
  }

  get ready() {
    return this.state.kind === "ready";
  }

  get busy() {
    return this.ready && !!this.status?.busy;
  }

  // Send a prompt to the agent. Returns false if the connection is not ready.
  prompt(text: string): boolean {
    return this.#send({ type: "prompt", id: crypto.randomUUID(), text });
  }

  cancel() {
    this.#send({ type: "cancel" });
  }

  answer(item: PermissionItem, option: PermissionOption) {
    if (!this.#send({ type: "permission", request: item.request, option: option.id })) return;
    item.answer = option.name;
    this.#changed();
  }

  // A phone stops the WebSocket of a page in the background. Connect again when the page shows.
  wake() {
    this.#connection?.wake();
  }

  #send(message: ToLaptop): boolean {
    return this.#connection?.send(message) ?? false;
  }

  #showPairing(error = "") {
    this.#connection?.close();
    this.#connection = null;
    this.screen = "pairing";
    this.pairError = error;
    this.#changed();
  }

  async #connect(laptop: Laptop, pair?: string) {
    this.#connection?.close();
    this.timeline.clear();
    this.status = null;
    this.state = { kind: "connecting" };
    this.screen = "chat";
    this.name = laptop.name;
    this.#changed();
    const { device, key } = await identity();
    const current: Connection = new Connection({
      relay: laptop.relay,
      laptop: laptop.laptop,
      laptopKey: laptop.key,
      device,
      key,
      pair,
      onState: (next) => {
        if (this.#connection !== current) return;
        this.state = next;
        if (next.kind === "stopped" && next.unpaired) {
          void forgetLaptop();
          return this.#showPairing(next.reason);
        }
        this.#changed();
      },
      onReady: (name) => {
        this.name = name;
        void saveLaptop({ ...laptop, name });
        current.send({ type: "sync", after: this.timeline.seq });
      },
      onMessage: (message) => this.#receive(message),
    });
    this.#connection = current;
  }

  #receive(message: ToPhone) {
    switch (message.type) {
      case "status":
        this.status = message;
        break;
      case "event":
        this.timeline.apply(message.seq, message.event);
        break;
      case "reset":
        this.timeline.clear();
        this.timeline.seq = message.first - 1;
        break;
      case "error":
        this.timeline.items.push({ kind: "error", message: message.message });
        break;
      case "accepted":
        return;
    }
    this.#changed();
  }
}

export const remote = new Remote();

// Render again when the remote state changes.
export function useRemote(): Remote {
  useSyncExternalStore(remote.subscribe, () => remote.version);
  return remote;
}
