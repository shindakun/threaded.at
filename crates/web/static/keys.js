// Single-key shortcuts, loaded only for readers who turned them on in
// /settings: j and k move focus to the next and previous story or comment,
// o opens the link of the story in focus, and Enter opens its comments.
// Keys typed into a form, or pressed with Ctrl, Alt, or Cmd, are left alone.
(() => {
  // Stories and comments in page order, skipping any inside a folded thread.
  function items() {
    return [...document.querySelectorAll("main .story, main article.comment")].filter(
      (el) => el.getClientRects().length > 0,
    );
  }

  function move(step) {
    const list = items();
    if (list.length === 0) return;
    const at = list.findIndex((el) => el.contains(document.activeElement));
    let next;
    if (at < 0) next = step > 0 ? 0 : list.length - 1;
    else next = Math.min(Math.max(at + step, 0), list.length - 1);
    const el = list[next];
    if (!el.hasAttribute("tabindex")) el.setAttribute("tabindex", "-1");
    el.focus();
  }

  function open() {
    const story = document.activeElement?.closest(".story");
    const link = story?.querySelector("a.title");
    if (!link) return false;
    link.click();
    return true;
  }

  // Enter acts only on the story row itself. On a link or a button inside
  // the row, Enter does what it always does.
  function comments() {
    const story = document.activeElement;
    if (!story?.matches(".story")) return false;
    const link = story.querySelector(".meta a[href^='/s/']");
    if (!link) return false;
    link.click();
    return true;
  }

  document.addEventListener("keydown", (e) => {
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
    const t = e.target;
    if (t instanceof Element && (t.closest("input, textarea, select") || t.isContentEditable)) return;
    let done = true;
    if (e.key === "j") move(1);
    else if (e.key === "k") move(-1);
    else if (e.key === "o") done = open();
    else if (e.key === "Enter") done = comments();
    else done = false;
    if (done) e.preventDefault();
  });
})();
