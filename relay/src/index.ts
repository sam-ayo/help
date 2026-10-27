// The relay between the phones and the laptops. The phone and the laptop both open a WebSocket
// to the relay, and the relay passes the binary frames between them. The frames have the
// end-to-end encryption of `pwa/src/noise.ts`, so the relay cannot read them. The relay sees only
// the laptop IDs, the device IDs, and the sizes and times of the frames.
//
// Each laptop has one Durable Object, `Laptop`. It holds the WebSocket of the laptop and the
// WebSockets of its phones.
//
// Addresses:
//
//   GET /v1/laptop/<laptop>            with `Authorization: Bearer <secret>`
//   GET /v1/phone/<laptop>/<device>
//
// The first connection of a laptop ID keeps the SHA-256 of the secret. Each later connection of
// that laptop must have the same secret. A new laptop connection closes the old one.
//
// Frames between the laptop and the relay:
//
//   binary  [length of the device ID: 1 byte][device ID][frame of the phone]
//           In the two directions. The relay sends the frame to the phone, or from the phone.
//   text    JSON control messages. See `ToRelay` and `FromRelay`.
//
// Frames between a phone and the relay:
//
//   binary  A frame for the laptop, or from the laptop.
//   text    From the relay only: `{"type":"laptop","online":<boolean>}`.
//
// A phone can connect only if its device ID is in the device list of the laptop, or during a
// pairing period that the laptop opens. When the relay refuses or closes a connection, the
// close code tells why. See `Close`.

import { DurableObject } from "cloudflare:workers";

export interface Env {
  LAPTOP: DurableObjectNamespace<Laptop>;
  CONNECTIONS: RateLimit;
}

// Control messages from the laptop to the relay.
export type ToRelay =
  // The device IDs of the paired phones. The laptop sends it after each connection and after
  // each change. The relay closes the connections of the phones that are not in the list.
  | { type: "devices"; ids: string[] }
  // Let phones that are not in the list connect, for `seconds`. 0 stops the pairing period.
  | { type: "pairing"; seconds: number }
  // Close the connection of a phone, for example after a failed handshake.
  | { type: "close"; device: string };

// Control messages from the relay to the laptop.
export type FromRelay =
  // A phone connected. `pairing` is true if the device ID is not in the device list. A second
  // `open` for the same device, without `closed` before it, is a new connection that replaced
  // the old one. The phone starts a new handshake on each connection, and after each
  // `{"type":"laptop","online":true}`.
  | { type: "open"; device: string; pairing: boolean }
  | { type: "closed"; device: string };

export const Close = {
  // The laptop connected again, from a different connection.
  REPLACED: 4000,
  // The frame or the control message is not correct.
  BAD_FRAME: 4400,
  // The secret of the laptop is not correct.
  BAD_SECRET: 4401,
  // The device is not in the device list, and the laptop does not pair now.
  NOT_PAIRED: 4403,
  // No laptop has this ID.
  NO_LAPTOP: 4404,
  // Too many connections, pairing attempts, or frames.
  RATE_LIMIT: 4429,
} as const;

const ID = /^[A-Za-z0-9_-]{16,64}$/;
// A Noise message is 65535 bytes at most. The laptop frame adds the device ID.
const MAX_FRAME = 65535 + 1 + 64;
const MAX_CONTROL = 64 * 1024;
const MAX_DEVICES = 50;
const MAX_PHONES = 20;
const MAX_PAIRING_SECONDS = 10 * 60;
// The limits for each laptop, for each minute.
const PHONE_CONNECTIONS_PER_MINUTE = 30;
const LAPTOP_CONNECTIONS_PER_MINUTE = 10;
// The unknown devices that can connect in one pairing period.
const PAIRING_ATTEMPTS = 5;
// The frames from one connection, for each 10 seconds.
const FRAMES_PER_10_SECONDS = 1000;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/") return new Response("help relay\n");
    const parts = url.pathname.split("/");
    const laptop = parts[3];
    const role = parts[1] === "v1" ? parts[2] : undefined;
    const valid =
      (role === "laptop" && parts.length === 4 && ID.test(laptop)) ||
      (role === "phone" && parts.length === 5 && ID.test(laptop) && ID.test(parts[4]));
    if (!valid) return new Response("not found\n", { status: 404 });
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("use a WebSocket\n", { status: 426 });
    }
    const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
    if (!(await env.CONNECTIONS.limit({ key: ip })).success) {
      return new Response("too many connections\n", { status: 429 });
    }
    return env.LAPTOP.get(env.LAPTOP.idFromName(laptop)).fetch(request);
  },
} satisfies ExportedHandler<Env>;

interface Attachment {
  role: "laptop" | "phone";
  // The device ID of a phone.
  device?: string;
  // True for a phone that connected during a pairing period and is not in the device list.
  pairing?: boolean;
}

// A count of events in a period. The count goes back to 0 when the Durable Object hibernates.
// That is not a problem: the object does not hibernate while a client sends a lot of requests.
class Counter {
  #limit: number;
  #period: number;
  #start = 0;
  #count = 0;

  constructor(limit: number, period: number) {
    this.#limit = limit;
    this.#period = period;
  }

  // Count one event. Returns false if the limit is reached.
  take(): boolean {
    const now = Date.now();
    if (now - this.#start >= this.#period) {
      this.#start = now;
      this.#count = 0;
    }
    return ++this.#count <= this.#limit;
  }
}

export class Laptop extends DurableObject<Env> {
  #phoneConnections = new Counter(PHONE_CONNECTIONS_PER_MINUTE, 60_000);
  #laptopConnections = new Counter(LAPTOP_CONNECTIONS_PER_MINUTE, 60_000);
  #frames = new WeakMap<WebSocket, Counter>();

  async fetch(request: Request): Promise<Response> {
    const parts = new URL(request.url).pathname.split("/");
    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    const reject = (code: number, reason: string) => {
      server.accept();
      server.close(code, reason);
      return new Response(null, { status: 101, webSocket: client });
    };

    if (parts[2] === "laptop") {
      if (!this.#laptopConnections.take()) return reject(Close.RATE_LIMIT, "too many connections");
      const auth = request.headers.get("Authorization") ?? "";
      const secret = auth.startsWith("Bearer ") ? auth.slice(7) : "";
      if (secret.length < 16) return reject(Close.BAD_SECRET, "no secret");
      const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret)));
      const stored = await this.ctx.storage.get<Uint8Array>("secret");
      if (!stored) {
        await this.ctx.storage.put("secret", hash);
      } else if (!crypto.subtle.timingSafeEqual(stored, hash)) {
        return reject(Close.BAD_SECRET, "wrong secret");
      }
      for (const old of this.ctx.getWebSockets("laptop")) old.close(Close.REPLACED, "replaced");
      this.ctx.acceptWebSocket(server, ["laptop"]);
      server.serializeAttachment({ role: "laptop" } satisfies Attachment);
      for (const phone of this.ctx.getWebSockets("phone")) {
        const { device, pairing } = phone.deserializeAttachment() as Attachment;
        this.#control(server, { type: "open", device: device!, pairing: !!pairing });
        phone.send(JSON.stringify({ type: "laptop", online: true }));
      }
      return new Response(null, { status: 101, webSocket: client });
    }

    const device = parts[4];
    if (!this.#phoneConnections.take()) return reject(Close.RATE_LIMIT, "too many connections");
    if (!(await this.ctx.storage.get("secret"))) return reject(Close.NO_LAPTOP, "no laptop");
    const devices = (await this.ctx.storage.get<string[]>("devices")) ?? [];
    const pairing = !devices.includes(device);
    if (pairing) {
      const until = (await this.ctx.storage.get<number>("pairingUntil")) ?? 0;
      if (Date.now() >= until) return reject(Close.NOT_PAIRED, "not paired");
      const attempts = (await this.ctx.storage.get<number>("pairingAttempts")) ?? 0;
      if (attempts >= PAIRING_ATTEMPTS) return reject(Close.RATE_LIMIT, "too many pairing attempts");
      await this.ctx.storage.put("pairingAttempts", attempts + 1);
    }
    const old = this.ctx.getWebSockets(`phone:${device}`);
    if (this.ctx.getWebSockets("phone").length - old.length >= MAX_PHONES) {
      return reject(Close.RATE_LIMIT, "too many phones");
    }
    // A phone that connects again closes its old connection, for example after a change of
    // network.
    for (const ws of old) ws.close(Close.REPLACED, "replaced");
    this.ctx.acceptWebSocket(server, ["phone", `phone:${device}`]);
    server.serializeAttachment({ role: "phone", device, pairing } satisfies Attachment);
    const laptop = this.#laptop();
    server.send(JSON.stringify({ type: "laptop", online: !!laptop }));
    if (laptop) this.#control(laptop, { type: "open", device, pairing });
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    let counter = this.#frames.get(ws);
    if (!counter) this.#frames.set(ws, (counter = new Counter(FRAMES_PER_10_SECONDS, 10_000)));
    if (!counter.take()) return ws.close(Close.RATE_LIMIT, "too many frames");

    const from = ws.deserializeAttachment() as Attachment;
    if (from.role === "phone") {
      if (typeof message === "string" || message.byteLength > 65535) {
        return ws.close(Close.BAD_FRAME, "send binary frames of 65535 bytes or less");
      }
      const id = new TextEncoder().encode(from.device!);
      const frame = new Uint8Array(1 + id.length + message.byteLength);
      frame[0] = id.length;
      frame.set(id, 1);
      frame.set(new Uint8Array(message), 1 + id.length);
      this.#laptop()?.send(frame);
      return;
    }

    if (typeof message !== "string") {
      const bytes = new Uint8Array(message);
      if (bytes.length > MAX_FRAME || bytes.length < 1 || bytes.length < 1 + bytes[0]) {
        return ws.close(Close.BAD_FRAME, "bad frame");
      }
      const device = new TextDecoder().decode(bytes.subarray(1, 1 + bytes[0]));
      // A frame for a phone that is not connected now. The laptop learns that from `closed`.
      for (const phone of this.ctx.getWebSockets(`phone:${device}`)) phone.send(bytes.subarray(1 + bytes[0]));
      return;
    }

    let control: ToRelay;
    try {
      if (message.length > MAX_CONTROL) throw new Error("too large");
      control = JSON.parse(message);
    } catch {
      return ws.close(Close.BAD_FRAME, "bad control message");
    }
    if (control.type === "devices") {
      const ids = control.ids;
      if (!Array.isArray(ids) || ids.length > MAX_DEVICES || !ids.every((id) => typeof id === "string" && ID.test(id))) {
        return ws.close(Close.BAD_FRAME, "bad device list");
      }
      await this.ctx.storage.put("devices", ids);
      const open = await this.#pairingOpen();
      for (const phone of this.ctx.getWebSockets("phone")) {
        const attachment = phone.deserializeAttachment() as Attachment;
        if (ids.includes(attachment.device!)) {
          if (attachment.pairing) phone.serializeAttachment({ ...attachment, pairing: false });
        } else if (!attachment.pairing || !open) {
          phone.close(Close.NOT_PAIRED, "not paired");
        }
      }
    } else if (control.type === "pairing") {
      const seconds = control.seconds;
      if (typeof seconds !== "number" || !(seconds >= 0 && seconds <= MAX_PAIRING_SECONDS)) {
        return ws.close(Close.BAD_FRAME, "bad pairing period");
      }
      await this.ctx.storage.put({ pairingUntil: Date.now() + seconds * 1000, pairingAttempts: 0 });
      if (seconds === 0) {
        for (const phone of this.ctx.getWebSockets("phone")) {
          if ((phone.deserializeAttachment() as Attachment).pairing) phone.close(Close.NOT_PAIRED, "not paired");
        }
      }
    } else if (control.type === "close") {
      if (typeof control.device !== "string") return ws.close(Close.BAD_FRAME, "bad device");
      for (const phone of this.ctx.getWebSockets(`phone:${control.device}`)) phone.close(Close.NOT_PAIRED, "closed by the laptop");
    } else {
      return ws.close(Close.BAD_FRAME, "unknown control message");
    }
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string): Promise<void> {
    this.#closed(ws);
    // The other side of the connection closed. Close this side too. 1005 and 1006 are not
    // codes that a close frame can have.
    try {
      ws.close(code === 1005 || code === 1006 ? 1000 : code, reason);
    } catch {
      // The connection is already closed.
    }
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    this.#closed(ws);
  }

  #closed(ws: WebSocket) {
    const from = ws.deserializeAttachment() as Attachment;
    if (from.role === "laptop") {
      // A replaced laptop connection closes after the new one is open. The phones stay online.
      if (this.#laptop(ws)) return;
      for (const phone of this.ctx.getWebSockets("phone")) phone.send(JSON.stringify({ type: "laptop", online: false }));
    } else {
      // A replaced phone connection closes after the new one is open. Tell the laptop only about
      // the last connection of a device.
      if (this.ctx.getWebSockets(`phone:${from.device}`).some((other) => other !== ws)) return;
      const laptop = this.#laptop();
      if (laptop) this.#control(laptop, { type: "closed", device: from.device! });
    }
  }

  // The open laptop connection, other than `except`.
  #laptop(except?: WebSocket): WebSocket | undefined {
    return this.ctx
      .getWebSockets("laptop")
      .find((ws) => ws !== except && ws.readyState === WebSocket.OPEN);
  }

  async #pairingOpen(): Promise<boolean> {
    return Date.now() < ((await this.ctx.storage.get<number>("pairingUntil")) ?? 0);
  }

  #control(laptop: WebSocket, message: FromRelay) {
    laptop.send(JSON.stringify(message));
  }
}
