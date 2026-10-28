// What the audit and the behavior checks share: where Chrome is, and which
// axe rules to run.
import { existsSync } from "node:fs";
import { delimiter, join } from "node:path";

export const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"];

// puppeteer needs a full path, not a command name.
export function findChrome() {
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
