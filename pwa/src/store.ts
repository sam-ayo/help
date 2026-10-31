// The data that the phone keeps in IndexedDB: its static key, its device ID, and the laptop that
// it paired with. IndexedDB can keep a CryptoKey, so the private key stays inside WebCrypto.

import { x25519 } from "@noble/curves/ed25519.js";
import { generateWebCryptoKey, memoryKey, webCryptoKey, type StaticKey } from "./keys.ts";
import { toBase64Url } from "./pairing.ts";

export interface Laptop {
  relay: string;
  laptop: string;
  key: Uint8Array;
  name: string;
}

interface Identity {
  device: string;
  publicKey: Uint8Array;
  // A CryptoKey, or the secret key bytes in a browser without X25519 in WebCrypto.
  privateKey: CryptoKey | Uint8Array;
}

const DB = "help";
const STORE = "phone";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function run<T>(mode: IDBTransactionMode, action: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await open();
  try {
    return await new Promise<T>((resolve, reject) => {
      const request = action(db.transaction(STORE, mode).objectStore(STORE));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

const get = <T>(key: string) => run<T | undefined>("readonly", (s) => s.get(key));
const put = (key: string, value: unknown) => run("readwrite", (s) => s.put(value, key));
const remove = (key: string) => run("readwrite", (s) => s.delete(key));

// The device ID and the static key of this phone. Makes them on the first call.
export async function identity(): Promise<{ device: string; key: StaticKey }> {
  let saved = await get<Identity>("identity");
  if (!saved) {
    const device = toBase64Url(crypto.getRandomValues(new Uint8Array(16)));
    try {
      const key = await generateWebCryptoKey();
      saved = { device, publicKey: key.publicKey, privateKey: key.privateKey };
    } catch {
      const secret = x25519.utils.randomSecretKey();
      saved = { device, publicKey: x25519.getPublicKey(secret), privateKey: secret };
    }
    await put("identity", saved);
  }
  const key =
    saved.privateKey instanceof Uint8Array ? memoryKey(saved.privateKey) : webCryptoKey(saved.privateKey, saved.publicKey);
  return { device: saved.device, key };
}

export const loadLaptop = () => get<Laptop>("laptop");
export const saveLaptop = (laptop: Laptop) => put("laptop", laptop);
export const forgetLaptop = () => remove("laptop");
