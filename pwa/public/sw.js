// The service worker keeps the files of the app, so the app opens without a network. The page
// gets the newest files from the network first, and the cache only when the network fails.
// The messages to the laptop go through a WebSocket, which the service worker does not see.
//
// The service worker also shows the notifications from the relay. A notification has only the
// kind of the event, not the conversation. See `relay/src/push.ts`.

const CACHE = "help-v1";

self.addEventListener("install", () => self.skipWaiting());

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

const NOTIFICATIONS = {
  turn_end: "The agent finished its turn.",
  permission: "The agent asks for your permission.",
};

self.addEventListener("push", (event) => {
  let kind = "turn_end";
  try {
    kind = event.data?.json().kind ?? kind;
  } catch {
    // A push with no JSON payload shows the default text.
  }
  event.waitUntil(
    self.registration.showNotification("Help", {
      body: NOTIFICATIONS[kind] ?? NOTIFICATIONS.turn_end,
      icon: "/icon.svg",
      // A new notification replaces the old one.
      tag: "help",
      renotify: true,
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((windows) => (windows.length ? windows[0].focus() : self.clients.openWindow("/"))),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET" || new URL(request.url).origin !== location.origin) return;
  event.respondWith(
    fetch(request)
      .then((response) => {
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE).then((cache) => cache.put(request, copy));
        }
        return response;
      })
      .catch(() => caches.match(request).then((cached) => cached ?? caches.match("/"))),
  );
});
