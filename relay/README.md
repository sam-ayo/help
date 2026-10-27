# Relay

The relay passes the encrypted frames between the phones (`pwa/`) and the laptops
(`desktop/crates/remote`). It runs on Cloudflare Workers with one Durable Object for each
laptop. `src/index.ts` describes the addresses, the frames, and the close codes.

```sh
npm install
npm run typecheck
npm test          # starts `wrangler dev` and connects test clients to it
npm run dev       # a local relay on http://localhost:8787
```

Deploy with `npm run deploy`. This needs `wrangler login` first.
