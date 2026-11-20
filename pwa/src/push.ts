// Notifications at the end of a turn that this phone started. The relay sends them with Web Push.
// See `relay/src/push.ts` and `public/sw.js`.

export const pushSupported = "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
export const iOS = /iPhone|iPad/.test(navigator.userAgent);

export async function subscription(): Promise<PushSubscription | null> {
  if (!pushSupported) return null;
  const registration = await navigator.serviceWorker.getRegistration();
  return (await registration?.pushManager.getSubscription()) ?? null;
}

async function pushKey(relay: string): Promise<string | null> {
  const response = await fetch(`${relay.replace(/^ws/, "http")}/v1/push`).catch(() => null);
  if (!response?.ok) return null;
  return ((await response.json()) as { key: string }).key;
}

// Ask for the permission and subscribe. Call it from a click: Safari shows the prompt only from a
// click, so this asks before the first `await`. Throws an error with a message for the user.
export async function subscribe(relay: string): Promise<PushSubscription> {
  if ((await Notification.requestPermission()) !== "granted") {
    throw new Error("The browser does not let this app show notifications. Change that in the settings of the browser.");
  }
  const key = await pushKey(relay);
  if (!key) throw new Error("The relay of this laptop does not send notifications.");
  const registration = await navigator.serviceWorker.ready;
  try {
    return await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  } catch (error) {
    throw new Error(`Cannot turn on the notifications: ${(error as Error).message}`);
  }
}
