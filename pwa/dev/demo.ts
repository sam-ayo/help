// `pnpm demo`: run the PWA on this computer, with no desktop app. It starts three parts:
//
//   1. The relay (`relay/`) in `wrangler dev`, on http://localhost:8787.
//   2. The PWA in the Vite dev server, on http://localhost:5173.
//   3. A test laptop (`dev/laptop.ts`) that connects to the relay and plays an agent.
//
// Then open the pairing link that it shows. The relay and the laptop keep their state in
// `.demo/`, so the browser stays paired. Delete `.demo/` to start again.

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { startLaptop } from "./laptop.ts";

const RELAY_PORT = 8787;
const PWA_PORT = 5173;
const root = fileURLToPath(new URL("..", import.meta.url));
const relayDir = fileURLToPath(new URL("../../relay", import.meta.url));
const wranglerBin = `${relayDir}/node_modules/.bin/wrangler`;
const demo = `${root}.demo`;

if (!existsSync(wranglerBin)) {
  console.error(`Install the relay first: cd ${relayDir} && npm install`);
  process.exit(1);
}

const wrangler = spawn(wranglerBin, ["dev", "--port", String(RELAY_PORT), "--persist-to", `${demo}/relay`], {
  cwd: relayDir,
  stdio: ["ignore", "pipe", "pipe"],
  env: { ...process.env, WRANGLER_SEND_METRICS: "false" },
});
const stop = () => {
  wrangler.kill();
  process.exit(0);
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);

let output = "";
await new Promise<void>((resolve, reject) => {
  const read = (data: Buffer) => {
    output += data;
    if (output.includes("Ready on")) resolve();
  };
  wrangler.stdout!.on("data", read);
  wrangler.stderr!.on("data", read);
  wrangler.on("exit", () => reject(new Error(`wrangler dev stopped:\n${output}`)));
});
console.log(`Relay: ws://localhost:${RELAY_PORT}`);

const server = await createServer({ root, server: { port: PWA_PORT, strictPort: true } });
await server.listen();
const pwa = `http://localhost:${PWA_PORT}`;
console.log(`PWA:   ${pwa}`);

const { link } = await startLaptop({
  relay: `ws://localhost:${RELAY_PORT}`,
  pwa,
  file: `${demo}/laptop.json`,
  log: (line) => console.log(`Laptop: ${line}`),
});
console.log(`\nOpen this pairing link in a browser on this computer. It is good for one phone, for 5 minutes:\n\n${link}\n`);
console.log(`A browser that paired before can open ${pwa} directly. Press Ctrl+C to stop.`);
