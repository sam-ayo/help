// The phone app. `remote.ts` keeps the state, and `app.tsx` shows it.

import { createRoot } from "react-dom/client";
import { App } from "./app.tsx";
import { remote } from "./remote.ts";
import "./index.css";

// A phone stops the WebSocket of a page in the background. Connect again when the page shows.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") remote.wake();
});
addEventListener("online", () => remote.wake());

if (import.meta.env.PROD && "serviceWorker" in navigator) void navigator.serviceWorker.register("/sw.js");

createRoot(document.getElementById("root")!).render(<App />);
void remote.start();
