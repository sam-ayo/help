// The phone app had a "Notify me" button that subscribed to Web Push. The button is gone, so the
// app removes a subscription that an older version made. See `relay/src/push.ts`.

// Unsubscribe the push subscription of this browser. Returns true if there was one.
export async function dropPushSubscription(): Promise<boolean> {
  if (!("serviceWorker" in navigator) || !("PushManager" in window)) return false;
  const registration = await navigator.serviceWorker.getRegistration();
  const subscription = await registration?.pushManager.getSubscription();
  if (!subscription) return false;
  await subscription.unsubscribe();
  return true;
}
