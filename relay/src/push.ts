// Web Push from the relay to the phones. The relay sends a short notification when a turn that a
// phone started ends, or when the agent asks for permission in that turn. The notification has
// only the kind of the event, so the push service and the relay do not see the conversation.
//
// The payload has the encryption of RFC 8291 (`aes128gcm`), and the request has a VAPID
// signature (RFC 8292). Both use WebCrypto only.

// The subscription of a phone, as `PushSubscription.toJSON()` gives it.
export interface Subscription {
  endpoint: string;
  keys: { p256dh: string; auth: string };
}

export interface Vapid {
  // The P-256 public key, 65 bytes in base64url.
  publicKey: string;
  // The P-256 private key `d`, 32 bytes in base64url.
  privateKey: string;
  // A `mailto:` or `https:` address for the push service.
  subject: string;
}

// The kinds of notification. The service worker of the PWA shows a text for each kind.
export type Kind = "turn_end" | "permission";

const MAX_ENDPOINT = 2048;
const B64URL = /^[A-Za-z0-9_-]+$/;
// The push service keeps a notification for this time while the phone is off.
const TTL_SECONDS = 3600;
const RECORD_SIZE = 4096;

export function isSubscription(value: unknown, allowHttp = false): value is Subscription {
  const sub = value as Subscription;
  if (typeof sub?.endpoint !== "string" || sub.endpoint.length > MAX_ENDPOINT) return false;
  if (typeof sub.keys?.p256dh !== "string" || typeof sub.keys?.auth !== "string") return false;
  if (!B64URL.test(sub.keys.p256dh) || !B64URL.test(sub.keys.auth)) return false;
  try {
    const url = new URL(sub.endpoint);
    return url.protocol === "https:" || (allowHttp && url.protocol === "http:");
  } catch {
    return false;
  }
}

export function toBase64Url(bytes: Uint8Array): string {
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromBase64Url(text: string): Uint8Array<ArrayBuffer> {
  const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

function concat(...parts: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

const text = (value: string) => new TextEncoder().encode(value) as Uint8Array<ArrayBuffer>;

async function hkdf(salt: Uint8Array<ArrayBuffer>, ikm: Uint8Array<ArrayBuffer>, info: Uint8Array<ArrayBuffer>, length: number) {
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  return new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info }, key, length * 8));
}

// Encrypt `payload` for one subscription. Returns the body of the push request.
export async function encrypt(payload: Uint8Array, sub: Subscription): Promise<Uint8Array<ArrayBuffer>> {
  const phonePublic = fromBase64Url(sub.keys.p256dh);
  const auth = fromBase64Url(sub.keys.auth);
  const phoneKey = await crypto.subtle.importKey("raw", phonePublic, { name: "ECDH", namedCurve: "P-256" }, false, []);
  const local = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
  const localPublic = new Uint8Array((await crypto.subtle.exportKey("raw", local.publicKey)) as ArrayBuffer);
  // The Workers types name the `public` field `$public`, but the runtime reads `public`.
  const ecdh = { name: "ECDH", public: phoneKey } as unknown as Parameters<SubtleCrypto["deriveBits"]>[0];
  const shared = new Uint8Array(await crypto.subtle.deriveBits(ecdh, local.privateKey, 256));

  const ikm = await hkdf(auth, shared, concat(text("WebPush: info\0"), phonePublic, localPublic), 32);
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const cek = await hkdf(salt, ikm, text("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, ikm, text("Content-Encoding: nonce\0"), 12);
  const key = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["encrypt"]);
  // One record. The 2 marks the last record.
  const record = concat(payload, Uint8Array.of(2));
  const sealed = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, record));

  const header = new Uint8Array(16 + 4 + 1);
  header.set(salt);
  new DataView(header.buffer).setUint32(16, RECORD_SIZE);
  header[20] = localPublic.length;
  return concat(header, localPublic, sealed);
}

// The `Authorization` header for a request to `endpoint`.
export async function vapidAuthorization(endpoint: string, vapid: Vapid): Promise<string> {
  const publicKey = fromBase64Url(vapid.publicKey);
  const jwk: JsonWebKey = {
    kty: "EC",
    crv: "P-256",
    x: toBase64Url(publicKey.subarray(1, 33)),
    y: toBase64Url(publicKey.subarray(33, 65)),
    d: vapid.privateKey,
  };
  const key = await crypto.subtle.importKey("jwk", jwk, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const json = (value: unknown) => toBase64Url(text(JSON.stringify(value)));
  const claims = { aud: new URL(endpoint).origin, exp: Math.floor(Date.now() / 1000) + 12 * 3600, sub: vapid.subject };
  const unsigned = `${json({ typ: "JWT", alg: "ES256" })}.${json(claims)}`;
  const signature = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, text(unsigned)));
  return `vapid t=${unsigned}.${toBase64Url(signature)}, k=${vapid.publicKey}`;
}

// Send one notification. The push service answers 404 or 410 for a subscription that is not
// good now.
export async function send(sub: Subscription, kind: Kind, vapid: Vapid): Promise<Response> {
  return fetch(sub.endpoint, {
    method: "POST",
    headers: {
      Authorization: await vapidAuthorization(sub.endpoint, vapid),
      "Content-Encoding": "aes128gcm",
      "Content-Type": "application/octet-stream",
      TTL: String(TTL_SECONDS),
      Urgency: kind === "permission" ? "high" : "normal",
    },
    body: await encrypt(text(JSON.stringify({ kind })), sub),
  });
}

// A new VAPID key pair, for `npm run vapid`.
export async function generateVapid(): Promise<{ publicKey: string; privateKey: string }> {
  const pair = (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign"])) as CryptoKeyPair;
  const publicKey = new Uint8Array((await crypto.subtle.exportKey("raw", pair.publicKey)) as ArrayBuffer);
  const jwk = (await crypto.subtle.exportKey("jwk", pair.privateKey)) as JsonWebKey;
  return { publicKey: toBase64Url(publicKey), privateKey: jwk.d! };
}
