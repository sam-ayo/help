# Relay

The relay passes the encrypted frames between the phones (`pwa/`) and the laptops
(`desktop/crates/remote`). The same Worker also serves the files of the PWA, so the phone app and
the relay have one address. It runs on Cloudflare Workers with one Durable Object for each
laptop. `src/index.ts` describes the addresses, the frames, and the close codes.

```sh
npm install
npm run typecheck
npm test          # starts `wrangler dev` and connects test clients to it
npm run dev       # a local relay and the PWA on http://localhost:8787
```

The scripts build the PWA first (`pnpm build` in `pwa/`), because the Worker serves `pwa/dist`.

Deploy with `npm run deploy`. This needs `wrangler login` first.

## Notifications

The relay sends a Web Push notification to a phone when a turn from that phone ends, or asks for
permission (`src/push.ts`). The notification has no text from the conversation. The relay needs a
VAPID key pair for this. Without the keys, it sends no notifications.

```sh
npm run vapid     # makes the keys, and shows the commands that give them to the relay
```

For `wrangler dev`, put the keys in `.dev.vars`. Git does not keep that file. You can also set
`VAPID_SUBJECT` to a `mailto:` or `https:` address, for the push services.
