// Tests of the relay in `wrangler dev`. Run them with `npm test`.

import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

const PORT = 8790 + Math.floor(Math.random() * 100);
const BASE = `ws://127.0.0.1:${PORT}/v1`;
let wrangler: ChildProcess;
let state: string;

before(async () => {
  state = mkdtempSync(join(tmpdir(), "help-relay-"));
  wrangler = spawn("npx", ["wrangler", "dev", "--ip", "127.0.0.1", "--port", String(PORT), "--persist-to", state], {
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, WRANGLER_SEND_METRICS: "false" },
  });
  let output = "";
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`wrangler dev did not start:\n${output}`)), 60_000);
    const read = (data: Buffer) => {
      output += data;
      if (output.includes("Ready on")) {
        clearTimeout(timer);
        resolve();
      }
    };
    wrangler.stdout!.on("data", read);
    wrangler.stderr!.on("data", read);
    wrangler.on("exit", () => reject(new Error(`wrangler dev stopped:\n${output}`)));
  });
});

after(() => {
  wrangler.kill();
  rmSync(state, { recursive: true, force: true });
});

const id = () => randomBytes(16).toString("base64url");

// A WebSocket client that keeps the frames it receives, for `next`.
class Client {
  ws: WebSocket;
  #frames: (string | Uint8Array)[] = [];
  #waiting: ((frame: string | Uint8Array) => void)[] = [];
  closed: Promise<number>;

  constructor(path: string, secret?: string) {
    this.ws = new WebSocket(`${BASE}/${path}`, secret ? ({ headers: { Authorization: `Bearer ${secret}` } } as never) : undefined);
    this.ws.binaryType = "arraybuffer";
    this.ws.onmessage = (event) => {
      const frame = typeof event.data === "string" ? event.data : new Uint8Array(event.data as ArrayBuffer);
      const waiter = this.#waiting.shift();
      if (waiter) waiter(frame);
      else this.#frames.push(frame);
    };
    this.closed = new Promise((resolve) => (this.ws.onclose = (event) => resolve(event.code)));
  }

  next(): Promise<string | Uint8Array> {
    const frame = this.#frames.shift();
    if (frame !== undefined) return Promise.resolve(frame);
    return new Promise((resolve, reject) => {
      this.#waiting.push(resolve);
      setTimeout(() => reject(new Error("no frame")), 5000);
    });
  }

  async json(): Promise<unknown> {
    const frame = await this.next();
    assert.equal(typeof frame, "string", "a text frame");
    return JSON.parse(frame as string);
  }

  async binary(): Promise<Uint8Array> {
    const frame = await this.next();
    assert.ok(frame instanceof Uint8Array, "a binary frame");
    return frame;
  }

  // No frame comes in a short time.
  async quiet() {
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(this.#frames, []);
  }

  send(data: string | Uint8Array<ArrayBuffer>) {
    this.ws.send(data);
  }

  open(): Promise<void> {
    if (this.ws.readyState === WebSocket.OPEN) return Promise.resolve();
    return new Promise((resolve, reject) => {
      this.ws.addEventListener("open", () => resolve());
      this.ws.addEventListener("close", () => reject(new Error("closed")));
    });
  }

  close() {
    this.ws.close();
  }
}

function laptopFrame(device: string, payload: Uint8Array): Uint8Array<ArrayBuffer> {
  const id = new TextEncoder().encode(device);
  return Uint8Array.from([id.length, ...id, ...payload]);
}

async function newLaptop() {
  const laptop = id();
  const secret = id();
  const client = new Client(`laptop/${laptop}`, secret);
  await client.open();
  return { laptop, secret, client };
}

test("a paired phone and the laptop send frames to each other", async () => {
  const { laptop, client: lap } = await newLaptop();
  const device = id();
  lap.send(JSON.stringify({ type: "devices", ids: [device] }));
  await lap.quiet();

  const phone = new Client(`phone/${laptop}/${device}`);
  assert.deepEqual(await phone.json(), { type: "laptop", online: true });
  assert.deepEqual(await lap.json(), { type: "open", device, pairing: false });

  phone.send(Uint8Array.of(1, 2, 3));
  assert.deepEqual(await lap.binary(), laptopFrame(device, Uint8Array.of(1, 2, 3)));
  lap.send(laptopFrame(device, Uint8Array.of(9, 8)));
  assert.deepEqual(await phone.binary(), Uint8Array.of(9, 8));

  phone.close();
  assert.deepEqual(await lap.json(), { type: "closed", device });
  lap.close();
});

test("an unknown device can connect only during a pairing period", async () => {
  const { laptop, client: lap } = await newLaptop();
  const device = id();
  assert.equal(await new Client(`phone/${laptop}/${device}`).closed, 4403);

  lap.send(JSON.stringify({ type: "pairing", seconds: 60 }));
  await lap.quiet();
  const phone = new Client(`phone/${laptop}/${device}`);
  assert.deepEqual(await phone.json(), { type: "laptop", online: true });
  assert.deepEqual(await lap.json(), { type: "open", device, pairing: true });

  // The laptop adds the phone to the list, and stops the pairing period. The phone stays.
  lap.send(JSON.stringify({ type: "devices", ids: [device] }));
  lap.send(JSON.stringify({ type: "pairing", seconds: 0 }));
  phone.send(Uint8Array.of(5));
  assert.deepEqual(await lap.binary(), laptopFrame(device, Uint8Array.of(5)));

  // A different unknown device cannot connect now.
  assert.equal(await new Client(`phone/${laptop}/${id()}`).closed, 4403);
  phone.close();
  lap.close();
});

test("the end of the pairing period closes an unpaired phone", async () => {
  const { laptop, client: lap } = await newLaptop();
  lap.send(JSON.stringify({ type: "pairing", seconds: 60 }));
  await lap.quiet();
  const phone = new Client(`phone/${laptop}/${id()}`);
  await phone.json();
  await lap.json();
  lap.send(JSON.stringify({ type: "pairing", seconds: 0 }));
  assert.equal(await phone.closed, 4403);
  lap.close();
});

test("a pairing period lets only a small number of unknown devices connect", async () => {
  const { laptop, client: lap } = await newLaptop();
  lap.send(JSON.stringify({ type: "pairing", seconds: 60 }));
  await lap.quiet();
  const phones = [];
  for (let i = 0; i < 5; i++) {
    const phone = new Client(`phone/${laptop}/${id()}`);
    await phone.json();
    phones.push(phone);
  }
  assert.equal(await new Client(`phone/${laptop}/${id()}`).closed, 4429);
  for (const phone of phones) phone.close();
  lap.close();
});

test("the laptop revokes a phone", async () => {
  const { laptop, client: lap } = await newLaptop();
  const [keep, revoke] = [id(), id()];
  lap.send(JSON.stringify({ type: "devices", ids: [keep, revoke] }));
  await lap.quiet();
  const kept = new Client(`phone/${laptop}/${keep}`);
  const revoked = new Client(`phone/${laptop}/${revoke}`);
  await kept.json();
  await revoked.json();
  lap.send(JSON.stringify({ type: "devices", ids: [keep] }));
  assert.equal(await revoked.closed, 4403);
  assert.equal(await new Client(`phone/${laptop}/${revoke}`).closed, 4403);
  await kept.quiet();
  kept.close();
  lap.close();
});

test("the phones see when the laptop goes offline and comes back", async () => {
  const { laptop, secret, client: lap } = await newLaptop();
  const device = id();
  lap.send(JSON.stringify({ type: "devices", ids: [device] }));
  await lap.quiet();
  lap.close();
  await lap.closed;

  const phone = new Client(`phone/${laptop}/${device}`);
  assert.deepEqual(await phone.json(), { type: "laptop", online: false });
  const again = new Client(`laptop/${laptop}`, secret);
  assert.deepEqual(await again.json(), { type: "open", device, pairing: false });
  assert.deepEqual(await phone.json(), { type: "laptop", online: true });

  again.close();
  assert.deepEqual(await phone.json(), { type: "laptop", online: false });
  phone.close();
});

test("a new laptop connection replaces the old one", async () => {
  const { laptop, secret, client: lap } = await newLaptop();
  const device = id();
  lap.send(JSON.stringify({ type: "devices", ids: [device] }));
  await lap.quiet();
  const phone = new Client(`phone/${laptop}/${device}`);
  await phone.json();
  await lap.json();

  const again = new Client(`laptop/${laptop}`, secret);
  assert.equal(await lap.closed, 4000);
  assert.deepEqual(await again.json(), { type: "open", device, pairing: false });
  await phone.json(); // online: true
  await phone.quiet();
  phone.send(Uint8Array.of(7));
  assert.deepEqual(await again.binary(), laptopFrame(device, Uint8Array.of(7)));
  phone.close();
  again.close();
});

test("the relay refuses a wrong secret and an unknown laptop", async () => {
  const { laptop, client: lap } = await newLaptop();
  assert.equal(await new Client(`laptop/${laptop}`, id()).closed, 4401);
  assert.equal(await new Client(`laptop/${laptop}`).closed, 4401);
  await lap.quiet();
  assert.equal(lap.ws.readyState, WebSocket.OPEN);
  lap.close();

  assert.equal(await new Client(`phone/${id()}/${id()}`).closed, 4404);
});

test("the relay closes a connection that sends a bad frame", async () => {
  const { laptop, client: lap } = await newLaptop();
  const device = id();
  lap.send(JSON.stringify({ type: "devices", ids: [device] }));
  await lap.quiet();
  const phone = new Client(`phone/${laptop}/${device}`);
  await phone.json();
  await lap.json();
  phone.send("text");
  assert.equal(await phone.closed, 4400);
  lap.send(JSON.stringify({ type: "devices", ids: ["bad id"] }));
  assert.equal(await lap.closed, 4400);
});

test("the relay answers 404 to an unknown address", async () => {
  const response = await fetch(`http://127.0.0.1:${PORT}/v1/other/${id()}`);
  assert.equal(response.status, 404);
  const plain = await fetch(`http://127.0.0.1:${PORT}/v1/laptop/${id()}`);
  assert.equal(plain.status, 426);
});
