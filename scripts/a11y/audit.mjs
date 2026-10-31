// usage: [A11Y_COOKIE=name=value] node audit.mjs BASE_URL PATH...
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import puppeteer from "puppeteer-core";
import { findChrome, tags } from "./chrome.mjs";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");
const inpage = new URL("./inpage.js", import.meta.url).pathname;

const chrome = findChrome();

const [base, ...paths] = process.argv.slice(2);

// Text spacing as WCAG 1.4.12 sets it; a reader's stylesheet may impose it.
const spacing = `* { line-height: 1.5 !important; letter-spacing: 0.12em !important; word-spacing: 0.16em !important; }
p { margin-bottom: 2em !important; }`;

// What axe does not test, by viewport width:
// - 320: reflow (1.4.10), 1280 CSS pixels at 400% zoom;
// - 640: resize text (1.4.4), 1280 at 200% zoom;
// - text spacing (1.4.12) at full width and at 320.
const layouts = [
  ["reflow at 320px", 320, null],
  ["200% zoom", 640, null],
  ["text spacing", 1280, spacing],
  ["text spacing at 320px", 320, spacing],
];

const browser = await puppeteer.launch({ executablePath: chrome, headless: true, args: ["--no-sandbox"] });

async function open(path, width, css, scheme = "light") {
  const page = await browser.newPage();
  // A11Y_COOKIE (name=value) audits the pages as a signed-in user.
  if (process.env.A11Y_COOKIE) {
    const [name, ...value] = process.env.A11Y_COOKIE.split("=");
    await page.setCookie({ name, value: value.join("="), url: base });
  }
  await page.setViewport({ width, height: 800 });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
  await page.goto(base + path, { waitUntil: "load" });
  if (css) await page.addStyleTag({ content: css });
  await page.addScriptTag({ path: inpage });
  return page;
}

// What axe does not test in each color scheme: the focus indicator at each
// Tab stop, the edges of fields and buttons, and text contrast on hover.
async function interaction(page) {
  const out = [];
  const stops = await page.evaluate(() => document.querySelectorAll("a[href], button, input, select, textarea, summary, [tabindex]").length);
  for (let i = 0; i <= stops; i++) {
    await page.keyboard.press("Tab");
    const at = await page.evaluate(() => window.a11y.focused());
    if (at?.again) break;
    if (at) out.push(...at.problems);
  }
  out.push(...(await page.evaluate(() => window.a11y.boundaries())));
  const kinds = await page.evaluateHandle(() => window.a11y.kinds());
  for (const el of (await kinds.getProperties()).values()) {
    await el.asElement().hover();
    const problem = await el.evaluate((e) => window.a11y.textContrast(e));
    if (problem) out.push(problem);
  }
  await page.mouse.move(0, 0);
  // One line for each problem, however many elements share it.
  return [...new Set(out)];
}

let failures = 0;
for (const scheme of ["light", "dark"]) {
  for (const path of paths) {
    let page, result;
    try {
      page = await open(path, 1280, null, scheme);
      await page.addScriptTag({ content: axeSource });
      result = await page.evaluate(
        (values) => window.axe.run(document, { runOnly: { type: "tag", values } }),
        tags,
      );
    } catch (e) {
      failures++;
      console.log(`${scheme} ${path}: ${e.message}`);
      await page?.close();
      continue;
    }
    for (const v of result.violations) {
      failures++;
      console.log(`${scheme} ${path} [${v.impact}] ${v.id}: ${v.help} (${v.nodes.length}) ${v.nodes[0].target.join(" ")}`);
    }
    let problems;
    try {
      problems = await interaction(page);
    } catch (e) {
      problems = [e.message];
    }
    for (const problem of problems) {
      failures++;
      console.log(`${scheme} ${path}: ${problem}`);
    }
    if (result.violations.length === 0 && problems.length === 0) console.log(`${scheme} ${path}: ok`);
    await page.close();
  }
}
// The layout checks open each page again. Opening the inbox or a story
// marks it read, so axe runs first and sees the unread markers.
for (const path of paths) {
  const before = failures;
  for (const [label, width, css] of layouts) {
    let page, problems;
    try {
      page = await open(path, width, css);
      problems = await page.evaluate(() => window.a11y.lost());
    } catch (e) {
      problems = [e.message];
    }
    for (const problem of problems) {
      failures++;
      console.log(`${label} ${path}: ${problem}`);
    }
    await page?.close();
  }
  if (failures === before) console.log(`layout ${path}: ok`);
}
await browser.close();
process.exit(failures ? 1 : 0);
