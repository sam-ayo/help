// The screens of the PWA: the pairing screen, and the chat with the agent on the laptop.

import { Chat } from "@/components/chat.tsx";
import { Pairing } from "@/components/pairing.tsx";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useRemote } from "./remote.ts";

export function App() {
  const remote = useRemote();
  return (
    <TooltipProvider>
      {remote.screen === "pairing" && <Pairing />}
      {remote.screen === "chat" && <Chat />}
    </TooltipProvider>
  );
}
