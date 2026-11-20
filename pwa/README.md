# PWA

The phone app of the remote control. It pairs with a laptop through a QR code, then sends
prompts to the agent on the laptop and shows the results live. The messages go through the
relay (`relay/`) with end-to-end encryption (`src/noise.ts`).

```sh
pnpm install
pnpm demo          # the relay, a test laptop, and the PWA on http://localhost:5173
pnpm dev           # the PWA only
pnpm build         # the files for a web server, in dist/
pnpm typecheck
pnpm test
pnpm test:interop  # the TypeScript phone against the Rust laptop
```

`pnpm demo` needs the relay packages: `cd ../relay && npm install`. It shows a pairing link.
Open the link in a browser on this computer. The test laptop answers each prompt with a short
turn. A prompt with the word "run" also asks for a permission. The demo keeps its state in
`.demo/`. Delete that folder to start again.

The PWA needs a secure origin (`https://`, or `http://localhost`) for WebCrypto and the
service worker. To use it on a phone, put `dist/` on an HTTPS host and use the deployed relay.
