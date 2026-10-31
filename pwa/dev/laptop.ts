// A test laptop for the demo (`dev/demo.ts`). It connects to the relay the same way as the
// desktop app will, pairs phones, and plays an agent: each prompt gets a short turn with a
// thought, a plan, tool calls, and an answer. A prompt with the word "run" also asks the phone
// for a permission.
//
// The state (the IDs, the key, and the paired phones) is in `.demo/laptop.json`, so a paired
// browser stays paired after the demo starts again.

import { randomBytes } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { x25519 } from "@noble/curves/ed25519.js";
import { memoryKey } from "../src/keys.ts";
import { accept, type Transport } from "../src/noise.ts";
import { fromBase64Url, toBase64Url, type PairingLink } from "../src/pairing.ts";
import { VERSION, type Event, type LaptopHello, type PhoneHello, type ToLaptop, type ToPhone } from "../src/protocol.ts";

interface Saved {
  laptop: string;
  secret: string;
  key: string;
  // The device ID and the public key of each paired phone.
  devices: Record<string, string>;
}

const PAIRING_SECONDS = 5 * 60;
const AGENT = "Demo agent";

export interface Options {
  // The WebSocket address of the relay, for example `ws://localhost:8787`.
  relay: string;
  // The address of the PWA, for the pairing link.
  pwa: string;
  // The file of the state.
  file: string;
  log(line: string): void;
}

export async function startLaptop({ relay, pwa, file, log }: Options): Promise<{ link: string }> {
  const saved = load(file);
  const key = memoryKey(fromBase64Url(saved.key));
  const code = randomBytes(12).toString("base64url");
  const pairingLink: PairingLink = { v: 1, relay, laptop: saved.laptop, key: toBase64Url(key.publicKey), code };
  const link = `${pwa}/#pair=${toBase64Url(new TextEncoder().encode(JSON.stringify(pairingLink)))}`;
  let codeUsed = false;

  // The connected phones. A phone has a transport after the handshake.
  const phones = new Map<string, { transport: Transport | null; queue: Promise<void> }>();
  // The events of the session. The sequence numbers start at a number of this run, so a phone
  // with events of an earlier run gets a `reset`.
  const base = Date.now() * 1000;
  const events: Event[] = [];
  const queue: string[] = [];
  let busy = false;
  let cancel: (() => void) | null = null;
  let nextRequest = 1;
  const permissions = new Map<number, (option: string | null) => void>();

  let ws: WebSocket;
  const sendTo = (device: string, message: ToPhone) => {
    const transport = phones.get(device)?.transport;
    if (!transport) return;
    const id = new TextEncoder().encode(device);
    for (const chunk of transport.seal(new TextEncoder().encode(JSON.stringify(message)))) {
      ws.send(frame(id, chunk));
    }
  };
  const broadcast = (message: ToPhone) => {
    for (const device of phones.keys()) sendTo(device, message);
  };
  const status = (): ToPhone => ({ type: "status", agent: AGENT, busy, queued: queue.length });
  const emit = (event: Event) => {
    events.push(event);
    broadcast({ type: "event", seq: base + events.length, event });
  };

  async function handshake(device: string, bytes: Uint8Array) {
    const phone = phones.get(device)!;
    const hello = await accept(key, bytes);
    const payload = JSON.parse(new TextDecoder().decode(hello.payload)) as PhoneHello;
    const known = saved.devices[device];
    let reply: LaptopHello;
    if (payload.v !== VERSION) {
      reply = { v: VERSION, accepted: false, name: "Demo laptop", reason: `Version ${payload.v} is not supported.` };
    } else if (known && known === toBase64Url(hello.phone)) {
      reply = { v: VERSION, accepted: true, name: "Demo laptop" };
    } else if (!known && payload.pair === code && !codeUsed) {
      codeUsed = true;
      saved.devices[device] = toBase64Url(hello.phone);
      save(file, saved);
      ws.send(JSON.stringify({ type: "devices", ids: Object.keys(saved.devices) }));
      ws.send(JSON.stringify({ type: "pairing", seconds: 0 }));
      log(`Paired a phone: ${payload.device}`);
      reply = { v: VERSION, accepted: true, name: "Demo laptop" };
    } else {
      reply = { v: VERSION, accepted: false, name: "Demo laptop", reason: "The pairing code is not correct or was used." };
    }
    const { message, transport } = await hello.reply(new TextEncoder().encode(JSON.stringify(reply)));
    ws.send(frame(new TextEncoder().encode(device), message));
    if (!reply.accepted) {
      ws.send(JSON.stringify({ type: "close", device }));
      return;
    }
    phone.transport = transport;
    sendTo(device, status());
  }

  function handle(device: string, message: ToLaptop) {
    switch (message.type) {
      case "sync": {
        const after = message.after;
        let from = after - base;
        if (from < 0 || from > events.length) {
          sendTo(device, { type: "reset", first: base + 1 });
          from = 0;
        }
        for (let i = from; i < events.length; i++) sendTo(device, { type: "event", seq: base + i + 1, event: events[i] });
        return;
      }
      case "prompt":
        sendTo(device, { type: "accepted", id: message.id });
        queue.push(message.text);
        broadcast(status());
        if (!busy) void runQueue();
        return;
      case "cancel":
        cancel?.();
        return;
      case "permission":
        permissions.get(message.request)?.(message.option);
        return;
    }
  }

  async function runQueue() {
    busy = true;
    while (queue.length > 0) {
      const text = queue.shift()!;
      broadcast(status());
      let cancelled = false;
      const stopped = new Promise<void>((resolve) => (cancel = () => ((cancelled = true), resolve())));
      const wait = async (ms: number) => {
        await Promise.race([new Promise((resolve) => setTimeout(resolve, ms)), stopped]);
        if (cancelled) throw new Error("cancelled");
      };
      try {
        await turn(text, wait, stopped);
        emit({ kind: "turn_ended", reason: "end_turn" });
      } catch (error) {
        if (!cancelled) throw error;
        for (const [request, answer] of permissions) {
          answer(null);
          emit({ kind: "permission_closed", request });
        }
        emit({ kind: "turn_ended", reason: "Cancelled" });
      }
      cancel = null;
    }
    busy = false;
    broadcast(status());
  }

  async function turn(text: string, wait: (ms: number) => Promise<void>, stopped: Promise<void>) {
    emit({ kind: "user_text", text });
    await wait(400);
    emit({ kind: "thought", text: "The user sent a message. I will read the project, then answer." });
    const plan = (done: number) => ({
      kind: "plan" as const,
      entries: ["Read the project", "Answer the user"].map((entry, i) => ({
        text: entry,
        status: i < done ? ("done" as const) : i === done ? ("running" as const) : ("pending" as const),
      })),
    });
    emit(plan(0));
    await wait(400);
    emit({ kind: "tool_call", id: "read", title: "Read README.md", tool_kind: "read", status: "running" });
    await wait(800);
    emit({ kind: "tool_update", id: "read", status: "done", detail: "# Help\n\nAn agent desk for the Mac." });

    if (/\brun\b/i.test(text)) {
      emit({ kind: "tool_call", id: "run", title: "Run `ls -la`", tool_kind: "execute", status: "pending" });
      const request = nextRequest++;
      const answer = new Promise<string | null>((resolve) => permissions.set(request, resolve));
      emit({
        kind: "permission",
        request,
        title: "Run `ls -la`?",
        options: [
          { id: "allow", name: "Allow", allow: true },
          { id: "reject", name: "Deny", allow: false },
        ],
      });
      const option = await Promise.race([answer, stopped.then(() => null)]);
      permissions.delete(request);
      emit({ kind: "permission_closed", request });
      await wait(0);
      if (option === "allow") {
        emit({ kind: "tool_update", id: "run", status: "running" });
        await wait(600);
        emit({ kind: "tool_update", id: "run", status: "done", detail: "README.md\ndesktop\npwa\nrelay" });
      } else {
        emit({ kind: "tool_update", id: "run", status: "failed", detail: "The user denied the permission." });
      }
    }

    emit(plan(1));
    const answer = `You said: "${text}". This is the demo laptop, so I only repeat your message. Send a message with the word "run" to see a permission request.`;
    for (const word of answer.split(/(?<= )/)) {
      await wait(40);
      emit({ kind: "agent_text", text: word });
    }
    emit(plan(2));
  }

  function connect() {
    ws = new WebSocket(`${relay}/v1/laptop/${saved.laptop}`, {
      headers: { Authorization: `Bearer ${saved.secret}` },
    } as never);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "devices", ids: Object.keys(saved.devices) }));
      if (!codeUsed) ws.send(JSON.stringify({ type: "pairing", seconds: PAIRING_SECONDS }));
    };
    ws.onmessage = (event) => {
      if (typeof event.data === "string") {
        const control = JSON.parse(event.data) as { type: "open" | "closed"; device: string };
        if (control.type === "open") phones.set(control.device, { transport: null, queue: Promise.resolve() });
        else phones.delete(control.device);
        return;
      }
      const bytes = new Uint8Array(event.data as ArrayBuffer);
      const device = new TextDecoder().decode(bytes.subarray(1, 1 + bytes[0]));
      const data = bytes.slice(1 + bytes[0]);
      const phone = phones.get(device);
      if (!phone) return;
      phone.queue = phone.queue
        .then(async () => {
          if (!phone.transport) return handshake(device, data);
          const message = phone.transport.open(data);
          if (message) handle(device, JSON.parse(new TextDecoder().decode(message)) as ToLaptop);
        })
        .catch((error) => {
          log(`Closed a phone connection: ${(error as Error).message}`);
          phones.delete(device);
          ws.send(JSON.stringify({ type: "close", device }));
        });
    };
    ws.onclose = (event) => {
      log(`The relay connection closed (${event.code}). Connecting again.`);
      phones.clear();
      setTimeout(connect, 1000);
    };
  }

  connect();
  return { link };
}

function frame(device: Uint8Array, data: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(1 + device.length + data.length);
  out[0] = device.length;
  out.set(device, 1);
  out.set(data, 1 + device.length);
  return out;
}

function load(file: string): Saved {
  if (existsSync(file)) return JSON.parse(readFileSync(file, "utf8")) as Saved;
  const saved: Saved = {
    laptop: randomBytes(16).toString("base64url"),
    secret: randomBytes(32).toString("base64url"),
    key: toBase64Url(x25519.utils.randomSecretKey()),
    devices: {},
  };
  save(file, saved);
  return saved;
}

function save(file: string, saved: Saved) {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, JSON.stringify(saved, null, 2) + "\n");
}
