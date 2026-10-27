// The end-to-end encryption between the phone and the laptop. The relay passes the messages,
// but it cannot read them. This is the same protocol as `desktop/crates/remote/src/noise.rs`:
// `Noise_IK_25519_ChaChaPoly_SHA256`, with the phone as the initiator.
//
//   <- s
//   ...
//   -> e, es, s, ss     phone hello
//   <- e, ee, se        laptop hello
//
// After the handshake, each message goes in one or more chunks. The first byte of the plaintext
// of a chunk is MORE or LAST.

import { chacha20poly1305 } from "@noble/ciphers/chacha.js";
import { hmac } from "@noble/hashes/hmac.js";
import { sha256 } from "@noble/hashes/sha2.js";
import { memoryKey, type StaticKey } from "./keys.ts";

export const PATTERN = "Noise_IK_25519_ChaChaPoly_SHA256";
export const PROLOGUE = new TextEncoder().encode("help remote 1");

const MAX_MESSAGE = 65535;
const TAG = 16;
const MAX_CHUNK = MAX_MESSAGE - TAG - 1;
// The largest message after the chunks join. The laptop cannot fill the memory of the phone.
export const MAX_PLAINTEXT = 16 * 1024 * 1024;
const MORE = 0;
const LAST = 1;
// After 2^64 - 1 messages Noise must stop. A connection never gets near this number.
const MAX_NONCE = Number.MAX_SAFE_INTEGER;

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

// HKDF of the Noise specification, with two outputs.
function hkdf(ck: Uint8Array, ikm: Uint8Array): [Uint8Array, Uint8Array] {
  const temp = hmac(sha256, ck, ikm);
  const one = hmac(sha256, temp, Uint8Array.of(1));
  const two = hmac(sha256, temp, concat(one, Uint8Array.of(2)));
  return [one, two];
}

class Cipher {
  #key: Uint8Array;
  #n = 0;

  constructor(key: Uint8Array) {
    this.#key = key;
  }

  #nonce(): Uint8Array {
    if (this.#n >= MAX_NONCE) throw new Error("the connection sent too many messages");
    const nonce = new Uint8Array(12);
    const view = new DataView(nonce.buffer);
    view.setUint32(4, this.#n % 2 ** 32, true);
    view.setUint32(8, Math.floor(this.#n / 2 ** 32), true);
    this.#n += 1;
    return nonce;
  }

  encrypt(ad: Uint8Array, plaintext: Uint8Array): Uint8Array {
    return chacha20poly1305(this.#key, this.#nonce(), ad).encrypt(plaintext);
  }

  // A failed decryption does not use a nonce, the same as in the Rust library `snow`. But the
  // caller must close the connection after a failure.
  decrypt(ad: Uint8Array, ciphertext: Uint8Array): Uint8Array {
    const n = this.#n;
    try {
      return chacha20poly1305(this.#key, this.#nonce(), ad).decrypt(ciphertext);
    } catch {
      this.#n = n;
      throw new Error("cannot decrypt the message");
    }
  }
}

class Symmetric {
  ck: Uint8Array;
  h: Uint8Array;
  cipher: Cipher | null = null;

  constructor() {
    const name = new TextEncoder().encode(PATTERN);
    this.h = name.length <= 32 ? concat(name, new Uint8Array(32 - name.length)) : sha256(name);
    this.ck = this.h;
  }

  mixHash(data: Uint8Array) {
    this.h = sha256(concat(this.h, data));
  }

  mixKey(ikm: Uint8Array) {
    const [ck, key] = hkdf(this.ck, ikm);
    this.ck = ck;
    this.cipher = new Cipher(key);
  }

  encryptAndHash(plaintext: Uint8Array): Uint8Array {
    const out = this.cipher ? this.cipher.encrypt(this.h, plaintext) : plaintext;
    this.mixHash(out);
    return out;
  }

  decryptAndHash(ciphertext: Uint8Array): Uint8Array {
    const out = this.cipher ? this.cipher.decrypt(this.h, ciphertext) : ciphertext;
    this.mixHash(ciphertext);
    return out;
  }

  split(): [Cipher, Cipher] {
    const [one, two] = hkdf(this.ck, new Uint8Array());
    return [new Cipher(one), new Cipher(two)];
  }
}

// A phone that sent its first message and waits for the reply of the laptop.
export interface Dial {
  message: Uint8Array;
  finish(reply: Uint8Array): Promise<{ payload: Uint8Array; transport: Transport }>;
}

// Make the first message of the phone. `laptop` is the static public key of the laptop, from
// the pairing link.
export async function dial(phone: StaticKey, laptop: Uint8Array, payload: Uint8Array): Promise<Dial> {
  if (laptop.length !== 32) throw new Error("the laptop key must have 32 bytes");
  const state = new Symmetric();
  state.mixHash(PROLOGUE);
  state.mixHash(laptop);

  const e = memoryKey();
  state.mixHash(e.publicKey);
  state.mixKey(await e.dh(laptop));
  const s = state.encryptAndHash(phone.publicKey);
  state.mixKey(await phone.dh(laptop));
  const body = state.encryptAndHash(payload);
  const message = concat(e.publicKey, s, body);
  if (message.length > MAX_MESSAGE) throw new Error("the phone hello is too large");

  let used = false;
  return {
    message,
    async finish(reply) {
      if (used) throw new Error("the handshake is complete");
      used = true;
      if (reply.length < 32 + TAG) throw new Error("bad laptop hello");
      const re = reply.subarray(0, 32);
      state.mixHash(re);
      state.mixKey(await e.dh(re));
      state.mixKey(await phone.dh(re));
      const payload = state.decryptAndHash(reply.subarray(32));
      const [send, receive] = state.split();
      return { payload, transport: new Transport(send, receive) };
    },
  };
}

// An encrypted channel after the handshake. The messages must arrive in the order that the
// peer sent them. A lost, changed, or repeated message makes `open` fail. Then close the
// connection.
export class Transport {
  #send: Cipher;
  #receive: Cipher;
  #partial: Uint8Array[] = [];
  #partialLength = 0;

  constructor(send: Cipher, receive: Cipher) {
    this.#send = send;
    this.#receive = receive;
  }

  // Encrypt a message. Send each chunk as one WebSocket message, in order.
  seal(plaintext: Uint8Array): Uint8Array[] {
    if (plaintext.length > MAX_PLAINTEXT) {
      throw new Error(`the message has ${plaintext.length} bytes, more than ${MAX_PLAINTEXT}`);
    }
    const chunks: Uint8Array[] = [];
    let at = 0;
    do {
      const piece = plaintext.subarray(at, at + MAX_CHUNK);
      at += piece.length;
      const flag = at >= plaintext.length ? LAST : MORE;
      chunks.push(this.#send.encrypt(new Uint8Array(), concat(Uint8Array.of(flag), piece)));
    } while (at < plaintext.length);
    return chunks;
  }

  // Decrypt a chunk. Returns the full message after its last chunk, and null before.
  open(chunk: Uint8Array): Uint8Array | null {
    if (chunk.length > MAX_MESSAGE) throw new Error("the chunk is too large");
    const plain = this.#receive.decrypt(new Uint8Array(), chunk);
    if (plain.length === 0) throw new Error("empty chunk");
    const data = plain.subarray(1);
    if (this.#partialLength + data.length > MAX_PLAINTEXT) {
      throw new Error(`the message has more than ${MAX_PLAINTEXT} bytes`);
    }
    this.#partial.push(data);
    this.#partialLength += data.length;
    if (plain[0] === MORE) return null;
    if (plain[0] !== LAST) throw new Error(`bad chunk flag ${plain[0]}`);
    const message = concat(...this.#partial);
    this.#partial = [];
    this.#partialLength = 0;
    return message;
  }
}

// Two transports that talk to each other, for the tests.
export function transportPairForTests(): [Transport, Transport] {
  const a = new Uint8Array(32).fill(1);
  const b = new Uint8Array(32).fill(2);
  return [new Transport(new Cipher(a), new Cipher(b)), new Transport(new Cipher(b), new Cipher(a))];
}
