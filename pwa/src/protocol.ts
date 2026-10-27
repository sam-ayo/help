// The messages between the phone and the laptop, inside the encryption of `noise.ts`. These are
// the same types as in `desktop/crates/remote/src/message.rs`.

export const VERSION = 1;

// The payload of the first handshake message of the phone.
export interface PhoneHello {
  v: number;
  device: string;
  // The one-time code of the pairing link. Only the first connection of a phone has it.
  pair?: string;
}

// The payload of the handshake reply of the laptop.
export interface LaptopHello {
  v: number;
  accepted: boolean;
  name: string;
  reason?: string;
}

export type ToLaptop =
  // A message for the agent. `id` is a random ID. The laptop sends it back in `accepted`.
  | { type: "prompt"; id: string; text: string }
  | { type: "cancel" }
  // The answer to a `permission` event. `option` is null to reject.
  | { type: "permission"; request: number; option: string | null }
  // Send the events after `after`. Send it after each connection, with the last `seq`, or 0.
  | { type: "sync"; after: number };

export type ToPhone =
  | { type: "status"; agent: string; busy: boolean; queued: number }
  | { type: "accepted"; id: string }
  | { type: "event"; seq: number; event: Event }
  // The laptop does not have all of the events after `sync.after`. Clear the view. The events
  // from `first` follow.
  | { type: "reset"; first: number }
  | { type: "error"; message: string };

export type Status = "pending" | "running" | "done" | "failed";

export interface PermissionOption {
  id: string;
  name: string;
  allow: boolean;
}

export type Event =
  | { kind: "user_text"; text: string }
  // A part of the answer of the agent. Join the parts that follow each other.
  | { kind: "agent_text"; text: string }
  | { kind: "thought"; text: string }
  | { kind: "tool_call"; id: string; title: string; tool_kind: string; status: Status; parent?: string }
  | { kind: "tool_update"; id: string; title?: string; status?: Status; detail?: string }
  | { kind: "plan"; entries: { text: string; status: Status }[] }
  | { kind: "permission"; request: number; title: string; options: PermissionOption[] }
  | { kind: "permission_closed"; request: number }
  | { kind: "turn_ended"; reason: string }
  | { kind: "error"; message: string };

export function encode(message: ToLaptop | PhoneHello): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(message));
}

export function decode<T extends ToPhone | LaptopHello>(bytes: Uint8Array): T {
  return JSON.parse(new TextDecoder().decode(bytes)) as T;
}
