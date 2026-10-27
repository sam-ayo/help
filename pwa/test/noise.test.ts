import assert from "node:assert/strict";
import { test } from "node:test";
import { generateWebCryptoKey, memoryKey } from "../src/keys.ts";
import { dial, transportPairForTests, type Transport } from "../src/noise.ts";

function receive(to: Transport, chunks: Uint8Array[]): Uint8Array {
  for (const chunk of chunks.slice(0, -1)) assert.equal(to.open(chunk), null);
  const message = to.open(chunks[chunks.length - 1]);
  assert.ok(message);
  return message;
}

test("both directions", () => {
  const [phone, laptop] = transportPairForTests();
  assert.deepEqual(receive(laptop, phone.seal(new TextEncoder().encode("prompt"))), new TextEncoder().encode("prompt"));
  assert.deepEqual(receive(phone, laptop.seal(new Uint8Array())), new Uint8Array());
});

test("large message in chunks", () => {
  const [phone, laptop] = transportPairForTests();
  const big = Uint8Array.from({ length: 200_000 }, (_, i) => i % 256);
  const chunks = laptop.seal(big);
  assert.equal(chunks.length, 4);
  assert.deepEqual(receive(phone, chunks), big);
});

test("changed message fails", () => {
  const [phone, laptop] = transportPairForTests();
  const [chunk] = phone.seal(new TextEncoder().encode("prompt"));
  chunk[3] ^= 1;
  assert.throws(() => laptop.open(chunk));
});

test("repeated message fails", () => {
  const [phone, laptop] = transportPairForTests();
  const [chunk] = phone.seal(new TextEncoder().encode("prompt"));
  laptop.open(chunk);
  assert.throws(() => laptop.open(chunk));
});

test("WebCrypto and memory keys agree", async () => {
  const a = await generateWebCryptoKey();
  const b = memoryKey();
  assert.deepEqual(await a.dh(b.publicKey), await b.dh(a.publicKey));
  assert.equal(a.privateKey.extractable, false);
});

test("the phone hello has the expected size", async () => {
  const hello = await dial(memoryKey(), memoryKey().publicKey, new Uint8Array(10));
  // e (32) + encrypted s (32 + 16) + encrypted payload (10 + 16)
  assert.equal(hello.message.length, 106);
});
