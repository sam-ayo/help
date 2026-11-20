// The chat with the agent on the laptop: the header, the timeline, and the composer.

import { useState } from "react";
import { ArrowUpIcon, BellIcon, BellRingIcon, SquareIcon } from "lucide-react";
import { TimelineItem } from "@/components/timeline-item.tsx";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import { Button } from "@/components/ui/button";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
} from "@/components/ui/message-scroller";
import { iOS, pushSupported } from "@/push.ts";
import { useRemote } from "@/remote.ts";

export function Chat() {
  return (
    <main className="flex h-dvh flex-col pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]">
      <Header />
      <Timeline />
      <Composer />
    </main>
  );
}

function Header() {
  const remote = useRemote();

  async function notify() {
    // Safari on iOS has Web Push only in an app on the Home Screen. The button tells how to do that.
    if (!pushSupported) {
      alert("To get notifications, add this app to the Home Screen: tap Share, then Add to Home Screen. Then open the app from the Home Screen.");
      return;
    }
    try {
      await remote.toggleNotifications();
    } catch (error) {
      alert((error as Error).message);
    }
  }

  return (
    <header className="flex items-center gap-2 border-b px-4 py-2">
      <strong className="min-w-0 flex-1 truncate text-sm font-semibold">{remote.name}</strong>
      {(pushSupported || iOS) && (
        <Button variant="ghost" size="sm" aria-pressed={remote.notifications} onClick={notify}>
          {remote.notifications ? <BellRingIcon /> : <BellIcon />}
          {remote.notifications ? "Notifications on" : "Notify me"}
        </Button>
      )}
      <Button
        variant="ghost"
        size="sm"
        onClick={() => {
          if (confirm("Unpair this phone from the laptop?")) void remote.unpair();
        }}
      >
        Unpair
      </Button>
    </header>
  );
}

function Timeline() {
  const remote = useRemote();
  const items = remote.timeline.items;

  return (
    <MessageScrollerProvider autoScroll defaultScrollPosition="end">
      <MessageScroller className="flex-1">
        <MessageScrollerViewport>
          <MessageScrollerContent className="mx-auto w-full max-w-2xl gap-4 px-4 py-4">
            {items.length === 0 && (
              <p className="m-auto text-center text-sm text-muted-foreground">
                {remote.ready ? "Send a message to the agent on your laptop." : ""}
              </p>
            )}
            {items.map((item, index) => (
              // The timeline only adds items at the end, so the index is a stable key.
              <MessageScrollerItem key={index} messageId={String(index)} scrollAnchor={item.kind === "user"}>
                <TimelineItem item={item} last={index === items.length - 1} />
              </MessageScrollerItem>
            ))}
            {remote.state.kind === "stopped" && (
              // The connection does not start again, so tell why at the end of the timeline.
              <MessageScrollerItem messageId="stopped">
                <Bubble variant="destructive">
                  <BubbleContent>{remote.state.reason}</BubbleContent>
                </Bubble>
              </MessageScrollerItem>
            )}
          </MessageScrollerContent>
        </MessageScrollerViewport>
        <MessageScrollerButton />
      </MessageScroller>
    </MessageScrollerProvider>
  );
}

function Composer() {
  const remote = useRemote();
  const [text, setText] = useState("");

  function send() {
    const prompt = text.trim();
    if (prompt && remote.prompt(prompt)) setText("");
  }

  return (
    <form
      className="mx-auto w-full max-w-2xl px-4 pt-2 pb-3"
      onSubmit={(event) => {
        event.preventDefault();
        send();
      }}
    >
      <InputGroup className="has-[[data-slot=input-group-control]:focus-visible]:border-input has-[[data-slot=input-group-control]:focus-visible]:ring-0">
        <InputGroupTextarea
          rows={2}
          placeholder="Message the agent"
          className="max-h-40 text-base md:text-sm"
          value={text}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
              event.preventDefault();
              send();
            }
          }}
        />
        <InputGroupAddon align="block-end" className="justify-end">
          {remote.busy && (
            <InputGroupButton variant="outline" size="icon-sm" aria-label="Stop" onClick={() => remote.cancel()}>
              <SquareIcon className="fill-current" />
            </InputGroupButton>
          )}
          <InputGroupButton type="submit" variant="default" size="icon-sm" aria-label="Send" disabled={!remote.ready || !text.trim()}>
            <ArrowUpIcon />
          </InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </form>
  );
}
