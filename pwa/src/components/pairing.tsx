// The screen of a phone that is not paired. The phone pairs from a QR code that opens a pairing
// link, or from a link that the user pastes.

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useRemote } from "@/remote.ts";

export function Pairing() {
  const remote = useRemote();
  const [link, setLink] = useState("");

  return (
    <main className="mx-auto flex min-h-dvh max-w-md flex-col justify-center gap-4 px-6 pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]">
      <h1 className="font-heading text-2xl font-semibold">Help</h1>
      <p className="text-muted-foreground">
        On your Mac, open the Help app and click Phone in the sidebar. Then scan the QR code with the camera of this phone.
      </p>
      <form
        className="flex flex-col gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          remote.pair(link.trim());
        }}
      >
        <label htmlFor="pair-link" className="text-sm text-muted-foreground">
          Or paste the pairing link here:
        </label>
        <div className="flex gap-2">
          <Input
            id="pair-link"
            type="url"
            placeholder="https://…/#pair=…"
            autoComplete="off"
            required
            value={link}
            onChange={(event) => setLink(event.target.value)}
          />
          <Button type="submit">Pair</Button>
        </div>
      </form>
      {remote.pairError && <p className="text-sm text-destructive">{remote.pairError}</p>}
    </main>
  );
}
