// Tests of the Web Push encryption and the VAPID signature, against the receiver side of
// RFC 8291 and RFC 8292. Run them with `npm test`.

import assert from "node:assert/strict";
import { test } from "node:test";
import { encrypt, fromBase64Url, generateVapid, isSubscription, toBase64Url, vapidAuthorization } from "../src/push.ts";

async function hkdf(salt: Uint8Array<ArrayBuffer>, ikm: Uint8Array<ArrayBuffer>, info: string | Uint8Array<ArrayBuffer>, length: number) {
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const bytes = typeof info === "string" ? new TextEncoder().encode(info) : info;
  return new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info: bytes }, key, length * 8));
}

// A phone: its key pair and its auth secret.
async function phone() {
  const pair = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"]);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const auth = crypto.getRandomValues(new Uint8Array(16));
  const subscription = {
    endpoint: "https://push.example.com/send/abc",
    keys: { p256dh: toBase64Url(publicKey), auth: toBase64Url(auth) },
  };
  return { pair, publicKey, auth, subscription };
}

// Decrypt a push body as the browser does.
async function decrypt(body: Uint8Array, receiver: Awaited<ReturnType<typeof phone>>): Promise<string> {
  const salt = body.slice(0, 16);
  assert.equal(new DataView(body.buffer, body.byteOffset).getUint32(16), 4096);
  const idLength = body[20];
  const senderPublic = body.slice(21, 21 + idLength);
  const sender = await crypto.subtle.importKey("raw", senderPublic, { name: "ECDH", namedCurve: "P-256" }, false, []);
  const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: sender }, receiver.pair.privateKey, 256));
  const info = new Uint8Array([...new TextEncoder().encode("WebPush: info\0"), ...receiver.publicKey, ...senderPublic]);
  const ikm = await hkdf(receiver.auth, shared, info, 32);
  const cek = await hkdf(salt, ikm, "Content-Encoding: aes128gcm\0", 16);
  const nonce = await hkdf(salt, ikm, "Content-Encoding: nonce\0", 12);
  const key = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["decrypt"]);
  const record = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, key, body.slice(21 + idLength)));
  assert.equal(record.at(-1), 2, "the last record");
  return new TextDecoder().decode(record.subarray(0, -1));
}

test("the phone decrypts the payload", async () => {
  const receiver = await phone();
  const body = await encrypt(new TextEncoder().encode('{"kind":"turn_end"}'), receiver.subscription);
  assert.equal(await decrypt(body, receiver), '{"kind":"turn_end"}');
});

test("the VAPID header has a good signature for the origin of the endpoint", async () => {
  const keys = await generateVapid();
  const header = await vapidAuthorization("https://push.example.com/send/abc", { ...keys, subject: "mailto:a@example.com" });
  const match = /^vapid t=([^.]+)\.([^.]+)\.([^,]+), k=(.+)$/.exec(header);
  assert.ok(match, header);
  const [, head, claims, signature, k] = match;
  assert.equal(k, keys.publicKey);
  const parsed = JSON.parse(new TextDecoder().decode(fromBase64Url(claims)));
  assert.equal(parsed.aud, "https://push.example.com");
  assert.equal(parsed.sub, "mailto:a@example.com");
  assert.ok(parsed.exp > Date.now() / 1000);
  const publicKey = await crypto.subtle.importKey("raw", fromBase64Url(k), { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
  const good = await crypto.subtle.verify(
    { name: "ECDSA", hash: "SHA-256" },
    publicKey,
    fromBase64Url(signature),
    new TextEncoder().encode(`${head}.${claims}`),
  );
  assert.ok(good);
});

test("a subscription must have an https endpoint and the two keys", () => {
  const keys = { p256dh: "AAAA", auth: "BBBB" };
  assert.ok(isSubscription({ endpoint: "https://push.example.com/x", keys }));
  assert.ok(!isSubscription({ endpoint: "http://push.example.com/x", keys }));
  assert.ok(isSubscription({ endpoint: "http://127.0.0.1:9/x", keys }, true));
  assert.ok(!isSubscription({ endpoint: "https://push.example.com/x", keys: { p256dh: "a b", auth: "c" } }));
  assert.ok(!isSubscription({ endpoint: "https://push.example.com/x" }));
  assert.ok(!isSubscription(null));
});
