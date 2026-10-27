import assert from "node:assert/strict";
import { test } from "node:test";
import { parsePairingLink, toBase64Url } from "../src/pairing.ts";

function url(link: object): string {
  return "https://pwa.test/#pair=" + toBase64Url(new TextEncoder().encode(JSON.stringify(link)));
}

const good = { v: 1, relay: "wss://relay.test", laptop: "lap", key: toBase64Url(new Uint8Array(32).fill(7)), code: "c" };

test("reads a link", () => {
  const { link, key } = parsePairingLink(url(good));
  assert.deepEqual(link, good);
  assert.deepEqual(key, new Uint8Array(32).fill(7));
});

test("refuses a bad link", () => {
  assert.throws(() => parsePairingLink("https://pwa.test/"));
  assert.throws(() => parsePairingLink(url({ ...good, v: 2 })));
  assert.throws(() => parsePairingLink(url({ ...good, relay: "http://relay.test" })));
  assert.throws(() => parsePairingLink(url({ ...good, key: toBase64Url(new Uint8Array(31)) })));
  assert.throws(() => parsePairingLink(url({ ...good, code: "" })));
});
