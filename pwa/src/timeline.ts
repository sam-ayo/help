// The items that the phone shows, made from the events of the laptop.

import type { Event, PermissionOption, Status } from "./protocol.ts";

export type Item =
  | { kind: "user"; text: string }
  | { kind: "agent"; text: string }
  | { kind: "thought"; text: string }
  | { kind: "tool"; id: string; title: string; toolKind: string; status: Status; detail?: string; parent?: string }
  | { kind: "plan"; entries: { text: string; status: Status }[] }
  | { kind: "permission"; request: number; title: string; options: PermissionOption[]; open: boolean; answer?: string }
  | { kind: "end"; reason: string }
  | { kind: "error"; message: string };

export class Timeline {
  items: Item[] = [];
  // The `seq` of the last event. Send it in `sync`.
  seq = 0;

  clear() {
    this.items = [];
    this.seq = 0;
  }

  apply(seq: number, event: Event) {
    if (seq <= this.seq) return;
    this.seq = seq;
    const last = this.items[this.items.length - 1];
    switch (event.kind) {
      case "user_text":
        return this.items.push({ kind: "user", text: event.text });
      case "agent_text":
      case "thought": {
        const kind = event.kind === "agent_text" ? "agent" : "thought";
        if (last?.kind === kind) last.text += event.text;
        else this.items.push({ kind, text: event.text });
        return;
      }
      case "tool_call":
        return this.items.push({
          kind: "tool",
          id: event.id,
          title: event.title,
          toolKind: event.tool_kind,
          status: event.status,
          parent: event.parent,
        });
      case "tool_update": {
        const tool = this.#find((item) => item.kind === "tool" && item.id === event.id);
        if (tool?.kind !== "tool") return;
        if (event.title !== undefined) tool.title = event.title;
        if (event.status !== undefined) tool.status = event.status;
        if (event.detail !== undefined) tool.detail = event.detail;
        return;
      }
      case "plan": {
        // A turn has one plan. A new plan replaces the plan of the same turn.
        const plan = this.#find((item) => item.kind === "plan", (item) => item.kind === "user");
        if (plan?.kind === "plan") plan.entries = event.entries;
        else this.items.push({ kind: "plan", entries: event.entries });
        return;
      }
      case "permission":
        return this.items.push({ kind: "permission", request: event.request, title: event.title, options: event.options, open: true });
      case "permission_closed": {
        const permission = this.permission(event.request);
        if (permission) permission.open = false;
        return;
      }
      case "turn_ended":
        return this.items.push({ kind: "end", reason: event.reason });
      case "error":
        return this.items.push({ kind: "error", message: event.message });
    }
  }

  permission(request: number): Extract<Item, { kind: "permission" }> | undefined {
    const item = this.#find((item) => item.kind === "permission" && item.request === request);
    return item?.kind === "permission" ? item : undefined;
  }

  // The last item that matches, before an item that matches `stop`.
  #find(match: (item: Item) => boolean, stop?: (item: Item) => boolean): Item | undefined {
    for (let i = this.items.length - 1; i >= 0; i--) {
      if (match(this.items[i])) return this.items[i];
      if (stop?.(this.items[i])) return undefined;
    }
    return undefined;
  }
}
