// Checks the page scripts in a real browser: votes and saves in place,
// comment folding and its fragment, keyboard shortcuts only once turned
// on, and the sign-in typeahead against a stand-in AppView. The folded,
// focused, and open-list states also get an axe audit.
//
// usage: node behavior.mjs BASE_URL COOKIE STORY_PATH
//   COOKIE is name=value for a plain reader whose upvotes are private.
//   STORY_PATH is a story with a comment that has replies.
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import puppeteer from "puppeteer-core";
import { findChrome, tags } from "./chrome.mjs";

const require = createRequire(import.meta.url);
const axeSource = readFileSync(require.resolve("axe-core/axe.min.js"), "utf8");

const [base, cookie, storyPath] = process.argv.slice(2);
// No request leaves the machine: every host but the test server fails to
// resolve, so a check that opened an outside link could not reach it.
const browser = await puppeteer.launch({
  executablePath: findChrome(),
  headless: true,
  args: ["--no-sandbox", "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1"],
});
let failures = 0;

function check(name, ok, detail = "") {
  if (ok) console.log(`ok: ${name}`);
  else {
    failures++;
    console.log(`FAILED: ${name}${detail ? `: ${detail}` : ""}`);
  }
}

async function open(path) {
  const page = await browser.newPage();
  const [name, ...value] = cookie.split("=");
  await page.setCookie({ name, value: value.join("="), url: base });
  await page.goto(base + path, { waitUntil: "load" });
  return page;
}

async function audit(page, label) {
  for (const scheme of ["light", "dark"]) {
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
    if (!(await page.evaluate(() => "axe" in window))) await page.addScriptTag({ content: axeSource });
    const result = await page.evaluate(
      (values) => window.axe.run(document, { runOnly: { type: "tag", values } }),
      tags,
    );
    const found = result.violations.map((v) => `${v.id} ${v.nodes[0].target.join(" ")}`);
    check(`axe, ${label}, ${scheme}`, found.length === 0, found.join("; "));
  }
}

// Runs one group of checks. A throw, such as a wait that times out, fails
// the group and the rest still run.
async function group(name, run) {
  try {
    await run();
  } catch (e) {
    check(name, false, e.message.split("\n")[0]);
  }
}

// A page that reloads loses this marker, so a check can tell an update in
// place from a form post that navigated.
const mark = (page) => page.evaluate(() => (window.__stayed = true));
const stayed = (page) => page.evaluate(() => window.__stayed === true);

// Votes in place.
await group("votes in place", async () => {
  const page = await open("/");
  const n = await page.evaluate(() =>
    [...document.querySelectorAll("ol.stories > li.story")].findIndex((li) =>
      li.querySelector('form.vote button[aria-pressed="false"]'),
    ),
  );
  const row = `ol.stories > li.story:nth-child(${n + 1})`;
  const pressed = (want) =>
    page.waitForFunction(
      (sel, want) => document.querySelector(sel).getAttribute("aria-pressed") === want,
      { timeout: 5000 },
      `${row} form.vote button`,
      want,
    );
  const score = () => page.$eval(`${row} [data-score]`, (el) => Number(el.textContent));
  const before = await score();
  await mark(page);
  await page.click(`${row} form.vote button`);
  await pressed("true");
  const after = await score();
  check("a vote updates in place", n >= 0 && (await stayed(page)) && after === before + 1, `${before} -> ${after}`);
  await page.click(`${row} form.vote button`);
  await pressed("false");
  check("a second click takes the vote back", (await stayed(page)) && (await score()) === before);
  await page.close();
});

// Saves in place.
await group("saves in place", async () => {
  const page = await open("/");
  const form = "ol.stories > li.story form.mark[action$=\"/save\"]";
  const title = await page.$eval("ol.stories > li.story:has(form.mark[action$=\"/save\"]) a.title", (a) => a.firstChild.textContent);
  await mark(page);
  await page.click(`${form} button`);
  await page.waitForFunction((sel) => document.querySelector(sel)?.textContent === "unsave", { timeout: 5000 }, `${form} [data-label]`);
  const saved = await page.evaluate(() => fetch("/saved").then((r) => r.text()));
  check("a save updates in place and lists the story", (await stayed(page)) && saved.includes(title), title);
  await page.click(`${form} button`);
  await page.waitForFunction((sel) => document.querySelector(sel)?.textContent === "save", { timeout: 5000 }, `${form} [data-label]`);
  check("a second click unsaves", await stayed(page));
  await page.close();
});

// Folding, and the fragment that keeps it.
await group("folding", async () => {
  const page = await open(storyPath);
  const parent = "section.comments li:has(> ol.thread) > article.comment";
  const id = await page.$eval(parent, (a) => a.id.slice(2));
  const button = `#c-${id} .fold`;
  const visible = (sel) => page.$eval(sel, (el) => el.getClientRects().length > 0);
  check("each comment has a fold button", (await page.$$("section.comments article.comment .fold")).length > 1);
  check("an open comment says fold", (await page.$eval(button, (b) => b.getAttribute("aria-expanded"))) === "true");
  await page.click(button);
  check("folding hides the replies", !(await visible(`li:has(> #c-${id}) > ol.thread`)));
  check("folding hides the text", !(await visible(`#c-${id} .prose`)));
  check("the head stays", await visible(`#c-${id} .comment-head`));
  check("the button says what it hides", /^unfold \(\d+ more\)$/.test(await page.$eval(`${button} [data-label]`, (s) => s.textContent)));
  check("the fragment holds the fold", (await page.evaluate(() => location.hash)) === `#fold=${id}`);
  await audit(page, "a folded comment");
  await page.reload({ waitUntil: "load" });
  check("a reload keeps the fold", (await page.$eval(button, (b) => b.getAttribute("aria-expanded"))) === "false");
  await page.click(button);
  check("unfolding shows the replies", await visible(`li:has(> #c-${id}) > ol.thread`));
  check("the fragment empties", (await page.evaluate(() => location.hash)) === "");
  await page.close();
});

// Shortcuts: off by default.
await group("shortcuts off", async () => {
  const page = await open("/");
  check("keys.js is not loaded by default", (await page.$("script[src=\"/static/keys.js\"]")) === null);
  await page.keyboard.press("j");
  check("j does nothing by default", await page.evaluate(() => !document.activeElement.closest("li.story")));
  await page.close();
});

// Shortcuts: turned on in settings, then j, k, and o.
await group("shortcuts on", async () => {
  const page = await open("/settings");
  await page.click("input[name=\"on\"]");
  await Promise.all([page.waitForNavigation(), page.click("form[action=\"/settings/keys\"] button")]);
  check("the settings form turns shortcuts on", (await page.$("script[src=\"/static/keys.js\"]")) !== null);

  await page.goto(`${base}/newest`, { waitUntil: "load" });
  const focused = () => page.evaluate(() => {
    const li = document.activeElement.closest("li.story");
    return li ? [...document.querySelectorAll("li.story")].indexOf(li) : -1;
  });
  await page.keyboard.press("j");
  check("j moves to the first story", (await focused()) === 0);
  await page.keyboard.press("j");
  check("j moves to the next story", (await focused()) === 1);
  await audit(page, "a story in focus");
  await page.keyboard.press("k");
  check("k moves back", (await focused()) === 0);
  const href = await page.$eval("li.story a.title", (a) => a.href);
  await Promise.all([page.waitForNavigation(), page.keyboard.press("o")]);
  check("o opens the story in focus", page.url() === href, `${page.url()} != ${href}`);

  await page.goto(base + storyPath, { waitUntil: "load" });
  await page.focus("#comment-text");
  await page.keyboard.press("j");
  check(
    "j types into a form as usual",
    await page.evaluate(() => document.activeElement.id === "comment-text" && document.activeElement.value === "j"),
  );
  await page.$eval("#comment-text", (t) => {
    t.value = "";
    t.blur();
  });

  // Fold a comment with replies; j then passes over the hidden replies and
  // goes on to what follows them.
  const parent = await page.$eval("section.comments li:has(> ol.thread) > article.comment", (a) => a.id);
  await page.click(`#${parent} .fold`);
  await page.evaluate(() => document.activeElement.blur());
  const visible = await page.evaluate(() =>
    [...document.querySelectorAll("main li.story, main article.comment")]
      .filter((el) => el.getClientRects().length > 0)
      .map((el) => el.id || el.className),
  );
  const seen = [];
  for (let i = 0; i < visible.length; i++) {
    await page.keyboard.press("j");
    seen.push(await page.evaluate(() => document.activeElement.id || document.activeElement.className));
  }
  check(
    "something follows the folded comment",
    visible.indexOf(parent) >= 0 && visible.indexOf(parent) < visible.length - 1,
    visible.join(" "),
  );
  check("j visits each visible item in order, past the folded replies", seen.join(" ") === visible.join(" "), seen.join(" "));
  await audit(page, "a comment in focus");
  await page.close();
});

// The sign-in typeahead, signed out, as someone signing in would see it.
await group("typeahead", async () => {
  const page = await browser.newPage();
  await page.goto(`${base}/login`, { waitUntil: "load" });
  const state = () =>
    page.evaluate(() => {
      const input = document.querySelector("#identifier");
      const list = document.querySelector("#identifier-suggestions");
      return {
        value: input.value,
        expanded: input.getAttribute("aria-expanded"),
        active: input.getAttribute("aria-activedescendant"),
        open: !list.hidden,
        options: [...list.querySelectorAll('[role="option"]')].map((o) => o.textContent.trim()),
        selected: [...list.querySelectorAll('[aria-selected="true"]')].map((o) => o.id),
        status: document.querySelector('[data-typeahead] [role="status"]').textContent,
      };
    });
  const listed = () => page.waitForFunction(() => !document.querySelector("#identifier-suggestions").hidden, { timeout: 5000 });

  check("the field becomes a combobox", (await page.$eval("#identifier", (i) => i.getAttribute("role"))) === "combobox");
  await page.type("#identifier", "@Al");
  await listed();
  let s = await state();
  check("typing lists the accounts found", s.expanded === "true" && s.options.join("|") === "Alice @alice.example|alfie.example @alfie.example", s.options.join("|"));
  check("a status line counts them", s.status === "2 accounts found", s.status);
  await audit(page, "the suggestion list open");

  await page.keyboard.press("ArrowDown");
  s = await state();
  check("down arrow highlights the first", s.active === "identifier-option-0" && s.selected.join() === "identifier-option-0");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowUp");
  check("up arrow goes back", (await state()).active === "identifier-option-0");
  await page.keyboard.press("Enter");
  s = await state();
  check("Enter fills the field and closes the list", s.value === "alice.example" && !s.open && s.expanded === "false", JSON.stringify(s));
  check("Enter on a suggestion does not submit the form", new URL(page.url()).pathname === "/login", page.url());

  await page.$eval("#identifier", (i) => (i.value = ""));
  await page.type("#identifier", "al");
  await listed();
  await page.keyboard.press("Escape");
  s = await state();
  check("Escape closes the list and keeps the text", !s.open && s.value === "al", JSON.stringify(s));

  await page.$eval("#identifier", (i) => (i.value = ""));
  await page.type("#identifier", "zz");
  await page.waitForFunction(
    () => document.querySelector('[data-typeahead] [role="status"]').textContent === "No matching accounts",
    { timeout: 5000 },
  );
  check("no match says so and lists nothing", !(await state()).open);
  await page.close();
});

await browser.close();
process.exit(failures ? 1 : 0);
