// `npm run vapid`: make the VAPID keys for Web Push, and show the commands that give them to
// the relay.

import { generateVapid } from "../src/push.ts";

const { publicKey, privateKey } = await generateVapid();
console.log(`echo ${publicKey} | npx wrangler secret put VAPID_PUBLIC_KEY`);
console.log(`echo ${privateKey} | npx wrangler secret put VAPID_PRIVATE_KEY`);
console.log(`\nFor wrangler dev, put these lines in .dev.vars:\n`);
console.log(`VAPID_PUBLIC_KEY=${publicKey}\nVAPID_PRIVATE_KEY=${privateKey}`);
