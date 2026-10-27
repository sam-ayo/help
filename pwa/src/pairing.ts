// The pairing link that the laptop shows as a QR code: `<pwa>/#pair=<data>`. `data` is the
// base64url text of a JSON `PairingLink`. The data is in the fragment of the URL, so the browser
// does not send it to the server of the PWA. See `desktop/crates/remote/src/pairing.rs`.

export interface PairingLink {
  v: number;
  // The WebSocket address of the relay.
  relay: string;
  // The ID of the laptop at the relay.
  laptop: string;
  // The static public key of the laptop, in base64url.
  key: string;
  // The one-time code. Send it in `PhoneHello.pair`.
  code: string;
}

export function fromBase64Url(text: string): Uint8Array {
  const b64 = text.replace(/-/g, "+").replace(/_/g, "/");
  return Uint8Array.from(atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4)), (c) => c.charCodeAt(0));
}

export function toBase64Url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function parsePairingLink(url: string): { link: PairingLink; key: Uint8Array } {
  const at = url.indexOf("#pair=");
  if (at < 0) throw new Error("the link has no #pair=");
  const link = JSON.parse(new TextDecoder().decode(fromBase64Url(url.slice(at + 6)))) as PairingLink;
  if (link.v !== 1) throw new Error(`pairing link version ${link.v} is not supported`);
  for (const field of ["relay", "laptop", "key", "code"] as const) {
    if (typeof link[field] !== "string" || link[field] === "") throw new Error(`the link has no ${field}`);
  }
  if (!link.relay.startsWith("wss://") && !link.relay.startsWith("ws://localhost")) {
    throw new Error("the relay address must use wss://");
  }
  const key = fromBase64Url(link.key);
  if (key.length !== 32) throw new Error("the laptop key must have 32 bytes");
  return { link, key };
}
