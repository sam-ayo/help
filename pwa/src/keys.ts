// The static X25519 key of the phone. The Noise code only needs the public key and a
// Diffie-Hellman function, so the private key can stay inside WebCrypto.

import { x25519 } from "@noble/curves/ed25519.js";

export interface StaticKey {
  publicKey: Uint8Array;
  dh(peer: Uint8Array): Promise<Uint8Array>;
}

// A key that the browser keeps. JavaScript code cannot read the private key. Save `privateKey`
// in IndexedDB: a CryptoKey keeps its `extractable: false` setting there.
export interface WebCryptoKey extends StaticKey {
  privateKey: CryptoKey;
}

export async function generateWebCryptoKey(): Promise<WebCryptoKey> {
  const pair = (await crypto.subtle.generateKey({ name: "X25519" }, false, ["deriveBits"])) as CryptoKeyPair;
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return webCryptoKey(pair.privateKey, publicKey);
}

export function webCryptoKey(privateKey: CryptoKey, publicKey: Uint8Array): WebCryptoKey {
  return {
    privateKey,
    publicKey,
    async dh(peer) {
      const key = await crypto.subtle.importKey("raw", peer as BufferSource, { name: "X25519" }, false, []);
      return new Uint8Array(await crypto.subtle.deriveBits({ name: "X25519", public: key }, privateKey, 256));
    },
  };
}

// A key in JavaScript memory. Use it for the ephemeral keys, for the tests, and for a browser
// without X25519 in WebCrypto.
export function memoryKey(secretKey: Uint8Array = x25519.utils.randomSecretKey()): StaticKey {
  return {
    publicKey: x25519.getPublicKey(secretKey),
    async dh(peer) {
      return x25519.getSharedSecret(secretKey, peer);
    },
  };
}
