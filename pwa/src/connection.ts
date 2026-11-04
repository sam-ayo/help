// The connection of the phone to the laptop, through the relay. It opens the WebSocket, does
// the Noise handshake each time that the laptop comes online, and connects again after a
// network failure. See `relay/src/index.ts` for the addresses and the close codes.

import type { StaticKey } from "./keys.ts";
import { dial, type Dial, type Transport } from "./noise.ts";
import { decode, encode, VERSION, type LaptopHello, type ToLaptop, type ToPhone } from "./protocol.ts";

export type State =
  | { kind: "connecting" }
  | { kind: "offline" }
  | { kind: "ready"; name: string }
  // The connection stopped and does not start again. `unpaired` is true if the laptop does not
  // know this phone.
  | { kind: "stopped"; reason: string; unpaired: boolean };

export interface Options {
  relay: string;
  laptop: string;
  laptopKey: Uint8Array;
  device: string;
  key: StaticKey;
  // The one-time code of a pairing link, for the first connection.
  pair?: string;
  onState(state: State): void;
  onMessage(message: ToPhone): void;
  // The handshake is complete. Send `sync` from here.
  onReady(name: string): void;
}

// The close codes of the relay that stop the connection.
const STOP: Record<number, { reason: string; unpaired: boolean }> = {
  4000: { reason: "This phone connected again from a different tab.", unpaired: false },
  4403: { reason: "The laptop does not know this phone. Pair it again.", unpaired: true },
  4404: { reason: "The relay does not know this laptop. Pair it again.", unpaired: true },
};

const MAX_DELAY = 30_000;

// The control messages from a phone to the relay. See `relay/src/index.ts`.
export type PhoneControl = { type: "push"; subscription: PushSubscriptionJSON | null };

export class Connection {
  #options: Options;
  #ws: WebSocket | null = null;
  #dial: Dial | null = null;
  #transport: Transport | null = null;
  // The frames go through this chain one at a time, because the handshake is async.
  #queue: Promise<void> = Promise.resolve();
  #delay = 1000;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #stopped = false;

  constructor(options: Options) {
    this.#options = options;
    // Connect after the constructor returns, because the callbacks can refer to this object.
    queueMicrotask(() => {
      if (!this.#stopped) this.#connect();
    });
  }

  // Send a message to the laptop. Returns false if the connection is not ready.
  send(message: ToLaptop): boolean {
    if (!this.#transport || this.#ws?.readyState !== WebSocket.OPEN) return false;
    for (const chunk of this.#transport.seal(encode(message))) this.#ws.send(chunk as BufferSource);
    return true;
  }

  // Send a control message to the relay, for example the push subscription. Returns false if
  // the connection is not ready.
  control(message: PhoneControl): boolean {
    if (!this.#transport || this.#ws?.readyState !== WebSocket.OPEN) return false;
    this.#ws.send(JSON.stringify(message));
    return true;
  }

  // Connect now, if the connection waits to connect again. Call it when the page shows again.
  wake() {
    if (this.#stopped || this.#ws) return;
    clearTimeout(this.#timer);
    this.#delay = 1000;
    this.#connect();
  }

  close() {
    this.#stopped = true;
    clearTimeout(this.#timer);
    this.#ws?.close(1000);
  }

  #connect() {
    const { relay, laptop, device } = this.#options;
    this.#options.onState({ kind: "connecting" });
    const ws = new WebSocket(`${relay}/v1/phone/${laptop}/${device}`);
    ws.binaryType = "arraybuffer";
    this.#ws = ws;
    ws.onmessage = (event) => {
      const data = event.data as string | ArrayBuffer;
      this.#queue = this.#queue.then(() => this.#frame(ws, data)).catch((error) => {
        console.error(error);
        ws.close(1000);
      });
    };
    ws.onclose = (event) => {
      if (this.#ws !== ws) return;
      this.#ws = null;
      this.#dial = null;
      this.#transport = null;
      if (this.#stopped) return;
      const stop = STOP[event.code];
      if (stop) return this.#stop(stop.reason, stop.unpaired);
      this.#options.onState({ kind: "connecting" });
      this.#timer = setTimeout(() => this.#connect(), this.#delay);
      this.#delay = Math.min(this.#delay * 2, MAX_DELAY);
    };
  }

  async #frame(ws: WebSocket, data: string | ArrayBuffer) {
    if (this.#ws !== ws) return;
    if (typeof data === "string") {
      const control = JSON.parse(data) as { type: string; online?: boolean };
      if (control.type !== "laptop") return;
      this.#transport = null;
      this.#dial = null;
      if (!control.online) return this.#options.onState({ kind: "offline" });
      const { key, laptopKey, pair } = this.#options;
      const hello = await dial(key, laptopKey, encode({ v: VERSION, device: deviceName(), pair }));
      if (this.#ws !== ws) return;
      this.#dial = hello;
      ws.send(hello.message as BufferSource);
      return;
    }

    const bytes = new Uint8Array(data);
    if (this.#transport) {
      const message = this.#transport.open(bytes);
      if (message) this.#options.onMessage(decode<ToPhone>(message));
      return;
    }
    if (!this.#dial) throw new Error("a frame before the handshake");
    const { payload, transport } = await this.#dial.finish(bytes);
    this.#dial = null;
    const hello = decode<LaptopHello>(payload);
    if (!hello.accepted) {
      return this.#stop(hello.reason ?? "The laptop refused this phone.", true);
    }
    // The laptop knows this phone now. The code is good for one pairing only.
    this.#options.pair = undefined;
    this.#transport = transport;
    this.#delay = 1000;
    this.#options.onState({ kind: "ready", name: hello.name });
    this.#options.onReady(hello.name);
  }

  #stop(reason: string, unpaired: boolean) {
    this.close();
    this.#options.onState({ kind: "stopped", reason, unpaired });
  }
}

function deviceName(): string {
  const agent = navigator.userAgent;
  if (/iPhone/.test(agent)) return "iPhone";
  if (/iPad/.test(agent)) return "iPad";
  if (/Android/.test(agent)) return "Android phone";
  return "Browser";
}
