// Packs the built plugin into dist/com.localstreamkit.streamdeck.streamDeckPlugin,
// with the licence in it: the plugin folder is copied to packing/ and
// LICENSE.md put beside its manifest there, so the folder in the repo
// isn't changed. The version is the manifest's own (raised by hand when
// the plugin changes). Run by `npm run pack`, after `npm run build`.
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, readFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

const UUID = "com.localstreamkit.streamdeck";
const here = (path) => fileURLToPath(new URL(`../${path}`, import.meta.url));
const source = here(`${UUID}.sdPlugin`);
const packing = here("packing");
const staged = here(`packing/${UUID}.sdPlugin`);
const cli = here("node_modules/@elgato/cli/bin/streamdeck.mjs");

if (!existsSync(`${source}/bin/plugin.js`)) {
	throw new Error("the plugin isn't built: npm run build");
}
rmSync(packing, { recursive: true, force: true });
// Not its logs (they're Stream Deck's, from running it linked).
cpSync(source, staged, { recursive: true, filter: (path) => !/[\\/]logs([\\/]|$)/.test(path.slice(source.length)) });
// Whoever gets the file gets the licence and its Required Notice too.
cpSync(here("../LICENSE.md"), `${staged}/LICENSE.md`);

const streamdeck = (...args) => execFileSync(process.execPath, [cli, ...args], { stdio: "inherit" });
streamdeck("validate", staged);
streamdeck("pack", staged, "--output", here("dist"), "--force");
console.log(`Packed version ${JSON.parse(readFileSync(`${staged}/manifest.json`, "utf8")).Version}.`);
