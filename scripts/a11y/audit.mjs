// usage: [A11Y_COOKIE=name=value] node audit.mjs BASE_URL PATH...
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import puppeteer from "puppeteer-core";
import { findChrome, tags } from "./chrome.mjs";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

const chrome = findChrome();

const [base, ...paths] = process.argv.slice(2);

const browser = await puppeteer.launch({ executablePath: chrome, headless: true, args: ["--no-sandbox"] });
let failures = 0;
for (const scheme of ["light", "dark"]) {
  for (const path of paths) {
    const page = await browser.newPage();
    // A11Y_COOKIE (name=value) audits the pages as a signed-in user.
    if (process.env.A11Y_COOKIE) {
      const [name, ...value] = process.env.A11Y_COOKIE.split("=");
      await page.setCookie({ name, value: value.join("="), url: base });
    }
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
    await page.goto(base + path, { waitUntil: "load" });
    await page.addScriptTag({ content: axeSource });
    const result = await page.evaluate(
      (values) => window.axe.run(document, { runOnly: { type: "tag", values } }),
      tags,
    );
    for (const v of result.violations) {
      failures++;
      console.log(`${scheme} ${path} [${v.impact}] ${v.id}: ${v.help} (${v.nodes.length}) ${v.nodes[0].target.join(" ")}`);
    }
    if (result.violations.length === 0) console.log(`${scheme} ${path}: ok`);
    await page.close();
  }
}
await browser.close();
process.exit(failures ? 1 : 0);
