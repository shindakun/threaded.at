// Checks that run inside the page. audit.mjs adds this script to each page
// and calls the functions on window.a11y.
window.a11y = (() => {
  // A computed color is "rgb(r, g, b)" or "rgba(r, g, b, a)". Any other form
  // is an error, so that a check cannot pass on a color it did not read.
  function parse(text) {
    const m = text.match(/^rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)$/);
    if (!m) throw new Error(`unread color ${text}`);
    return { r: +m[1], g: +m[2], b: +m[3], a: m[4] === undefined ? 1 : +m[4] };
  }

  // Relative luminance and contrast ratio, as WCAG 2.2 defines them.
  function luminance({ r, g, b }) {
    const lin = (c) => {
      c /= 255;
      return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  }
  function ratio(x, y) {
    const [hi, lo] = [luminance(x), luminance(y)].sort((p, q) => q - p);
    return (hi + 0.05) / (lo + 0.05);
  }

  // A color with alpha below 1, painted over an opaque color.
  function over(top, below) {
    const mix = (t, b) => t * top.a + b * (1 - top.a);
    return { r: mix(top.r, below.r), g: mix(top.g, below.g), b: mix(top.b, below.b), a: 1 };
  }

  // The opaque color behind an element: its own background and the
  // backgrounds of its ancestors, painted over the canvas.
  function background(el) {
    const layers = [];
    for (let e = el; e; e = e.parentElement) {
      const c = parse(getComputedStyle(e).backgroundColor);
      if (c.a > 0) layers.push(c);
      if (c.a === 1) break;
    }
    // The canvas takes the root background; with none, it is white in light
    // and the dark canvas color in dark.
    let color = layers.at(-1)?.a === 1 ? layers.pop() : canvas();
    while (layers.length) color = over(layers.pop(), color);
    return color;
  }
  function canvas() {
    const probe = document.createElement("div");
    probe.style.cssText = "position:absolute;background:Canvas";
    document.body.append(probe);
    const c = parse(getComputedStyle(probe).backgroundColor);
    probe.remove();
    return c;
  }

  const name = (e) =>
    e.tagName.toLowerCase() + (e.className && typeof e.className === "string" ? "." + e.className.split(" ")[0] : "");
  const label = (e) => `${name(e)} "${(e.textContent || e.value || e.getAttribute("aria-label") || "").trim().slice(0, 30)}"`;

  // Text that is lost: the page scrolls sideways, or a box that hides its
  // overflow cuts text off. Code blocks and tables may scroll on their own;
  // the reflow criterion exempts two-dimensional content.
  function lost() {
    const out = [];
    const root = document.documentElement;
    if (root.scrollWidth > root.clientWidth + 1) {
      const wide = [...document.querySelectorAll("body *")]
        .filter((e) => e.getBoundingClientRect().right > root.clientWidth + 1 && !e.closest("pre, table"))
        .filter((e, _, all) => !all.some((o) => o !== e && o.contains(e)));
      out.push(`the page scrolls sideways (${root.scrollWidth}px in ${root.clientWidth}px): ${wide.slice(0, 4).map(name).join(", ")}`);
    }
    for (const e of document.querySelectorAll("body *")) {
      const s = getComputedStyle(e);
      if (!["hidden", "clip"].includes(s.overflowX) && !["hidden", "clip"].includes(s.overflowY)) continue;
      // Visually hidden text is clipped on purpose.
      if (!e.textContent.trim() || e.getBoundingClientRect().width <= 1) continue;
      if (e.scrollWidth > e.clientWidth + 1 || e.scrollHeight > e.clientHeight + 1) {
        out.push(`${name(e)} cuts off "${e.textContent.trim().slice(0, 40)}"`);
      }
    }
    return out;
  }

  // The element that has keyboard focus, and what is wrong with how it shows.
  // An element seen before means the Tab order has gone round the page.
  // - 2.4.7: an outline or a box shadow marks it;
  // - 1.4.11: the outline has 3:1 against the color behind the element;
  // - 2.4.11: some part of it is on top, not under other content.
  function focused() {
    const el = document.activeElement;
    if (!el || el === document.body) return null;
    if (el.hasAttribute("data-a11y-seen")) return { again: true };
    el.setAttribute("data-a11y-seen", "");
    const out = [];
    const s = getComputedStyle(el);
    const outline = s.outlineStyle !== "none" && parseFloat(s.outlineWidth) > 0;
    if (!outline && s.boxShadow === "none") {
      out.push(`${label(el)} shows no focus indicator`);
    } else if (outline) {
      const r = ratio(over(parse(s.outlineColor), background(el.parentElement || el)), background(el.parentElement || el));
      if (r < 3) out.push(`${label(el)} has a focus outline at ${r.toFixed(2)}:1, under 3:1`);
    }
    const box = el.getBoundingClientRect();
    const points = [
      [box.left + box.width / 2, box.top + box.height / 2],
      [box.left + 1, box.top + 1],
      [box.right - 1, box.top + 1],
      [box.left + 1, box.bottom - 1],
      [box.right - 1, box.bottom - 1],
    ];
    const seen = points.some(([x, y]) => {
      const hit = document.elementFromPoint(x, y);
      return hit && (hit === el || el.contains(hit) || hit.contains(el) || hit.closest("label")?.control === el);
    });
    if (!seen) out.push(`${label(el)} is hidden under other content when it has focus`);
    return { problems: out };
  }

  // 1.4.11: a text field, a select, or a button that has a border or a fill
  // shows its edge at 3:1 against the color around it, by the border or by
  // the fill. A button without both is text, and the text contrast check
  // covers it.
  function boundaries() {
    const out = [];
    const fields = "input:not([type=hidden]):not([type=checkbox]):not([type=radio]), textarea, select, button";
    for (const el of document.querySelectorAll(fields)) {
      const box = el.getBoundingClientRect();
      if (box.width === 0 || box.height === 0) continue;
      const s = getComputedStyle(el);
      const around = background(el.parentElement);
      const fill = over(parse(s.backgroundColor), around);
      // A transparent border or fill is not drawn.
      const borders = ["Top", "Right", "Bottom", "Left"]
        .filter((side) => s[`border${side}Style`] !== "none" && parseFloat(s[`border${side}Width`]) > 0)
        .map((side) => parse(s[`border${side}Color`]))
        .filter((c) => c.a > 0)
        .map((c) => over(c, around));
      const filled = parse(s.backgroundColor).a > 0;
      const isButton = el.tagName === "BUTTON";
      if (isButton && borders.length === 0 && !filled) continue;
      const best = Math.max(ratio(fill, around), ...borders.map((b) => ratio(b, around)));
      if (best < 3) out.push(`${label(el)} has an edge at ${best.toFixed(2)}:1, under 3:1`);
    }
    return out;
  }

  // One link or button of each kind: tag, class, and the class of its parent.
  function kinds() {
    const groups = new Map();
    for (const el of document.querySelectorAll("a[href], button, summary")) {
      const box = el.getBoundingClientRect();
      if (box.width === 0 || box.height === 0 || !el.textContent.trim()) continue;
      const key = name(el) + " in " + (el.parentElement ? name(el.parentElement) : "");
      if (!groups.has(key)) groups.set(key, el);
    }
    return [...groups.values()];
  }

  // 1.4.3 for an element in its current state: 4.5:1, or 3:1 for large text
  // (24px, or 18.67px and bold).
  function textContrast(el) {
    const s = getComputedStyle(el);
    const behind = background(el);
    const r = ratio(over(parse(s.color), behind), behind);
    const size = parseFloat(s.fontSize);
    const large = size >= 24 || (size >= 18.66 && parseInt(s.fontWeight, 10) >= 700);
    const need = large ? 3 : 4.5;
    return r < need ? `${label(el)} has text at ${r.toFixed(2)}:1 on hover, under ${need}:1` : null;
  }

  return { lost, focused, boundaries, kinds, textContrast };
})();
