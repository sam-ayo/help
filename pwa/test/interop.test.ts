// The TypeScript phone against the Rust laptop (`desktop/crates/remote/examples/noise_peer.rs`).
// Run it with `npm run test:interop`, which builds the Rust program first.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { generateWebCryptoKey } from "../src/keys.ts";
import { dial } from "../src/noise.ts";
import { fromBase64Url, toBase64Url } from "../src/pairing.ts";
import { decode, encode, VERSION, type LaptopHello, type ToLaptop, type ToPhone } from "../src/protocol.ts";

const PEER = fileURLToPath(new URL("../../desktop/target/debug/examples/noise_peer", import.meta.url));

test("TypeScript phone and Rust laptop", async () => {
  const child = spawn(PEER, { stdio: ["pipe", "pipe", "inherit"] });
  const lines = createInterface({ input: child.stdout! })[Symbol.asyncIterator]();
  const next = async () => (await lines.next()).value as string;
  const write = (line: string) => child.stdin!.write(line + "\n");

  try {
    const laptopKey = fromBase64Url(await next());
    const phone = await generateWebCryptoKey();
    const hello = await dial(phone, laptopKey, encode({ v: VERSION, device: "Test phone", pair: "abc" }));
    write(toBase64Url(hello.message));
    const { payload, transport } = await hello.finish(fromBase64Url(await next()));
    assert.deepEqual(decode<LaptopHello>(payload), {
      v: VERSION,
      accepted: true,
      name: "Test laptop for Test phone",
      reason: "code abc",
    });

    const send = (message: ToLaptop) => write(transport.seal(encode(message)).map(toBase64Url).join(","));
    const receive = async () => {
      let message: Uint8Array | null = null;
      for (const chunk of (await next()).split(",")) message = transport.open(fromBase64Url(chunk));
      assert.ok(message, "the line has no last chunk");
      return decode<ToPhone>(message);
    };

    send({ type: "prompt", id: "p1", text: "hello" });
    assert.deepEqual(await receive(), { type: "accepted", id: "p1" });
    assert.deepEqual(await receive(), { type: "event", seq: 1, event: { kind: "agent_text", text: "echo: hello" } });

    send({ type: "sync", after: 4 });
    const big = await receive();
    assert.equal(big.type === "event" && big.event.kind === "agent_text" && big.event.text.length, 200_000);

    send({ type: "cancel" });
    assert.deepEqual(await receive(), { type: "status", agent: "Claude Code", busy: false, queued: 0 });
  } finally {
    child.kill();
  }
});
