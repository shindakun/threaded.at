// usage: [A11Y_COOKIE=name=value] node audit.mjs BASE_URL PATH...
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { delimiter, join } from "node:path";
import puppeteer from "puppeteer-core";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

// puppeteer needs a full path, not a command name.
function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  if (process.platform === "darwin") return "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
  const names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"];
  for (const dir of (process.env.PATH || "").split(delimiter)) {
    for (const name of names) {
      const path = join(dir, name);
      if (existsSync(path)) return path;
    }
  }
  throw new Error(`no Chrome found on PATH (${names.join(", ")}); set CHROME`);
}

const chrome = findChrome();

const [base, ...paths] = process.argv.slice(2);
const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"];

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
