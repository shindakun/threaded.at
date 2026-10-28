// Comment folding. Each comment gets a "fold" button that hides its text and
// its replies. Folded comments are kept in the URL fragment as
// #fold=ID,ID, so a reload or a shared link opens with the same comments
// folded. Without this script every comment stays open.
(() => {
  const PREFIX = "#fold=";

  function fromFragment() {
    if (!location.hash.startsWith(PREFIX)) return new Set();
    return new Set(location.hash.slice(PREFIX.length).split(",").filter((id) => /^[a-z0-9]+$/.test(id)));
  }

  function toFragment() {
    const ids = [...document.querySelectorAll("li.folded > article.comment")].map((a) => a.id.slice(2));
    const url = location.pathname + location.search + (ids.length ? PREFIX + ids.join(",") : "");
    history.replaceState(history.state, "", url);
  }

  function show(li, button, folded) {
    li.classList.toggle("folded", folded);
    button.setAttribute("aria-expanded", String(!folded));
    const replies = li.querySelectorAll("article.comment").length - 1;
    const label = button.querySelector("[data-label]");
    if (!folded) label.textContent = "fold";
    else if (replies === 0) label.textContent = "unfold";
    else label.textContent = `unfold (${replies} more)`;
  }

  const start = fromFragment();
  for (const article of document.querySelectorAll("section.comments li > article.comment")) {
    const li = article.parentElement;
    const head = article.querySelector(".comment-head, .deleted");
    if (!head) continue;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "linkish fold";
    const label = document.createElement("span");
    label.dataset.label = "";
    const which = document.createElement("span");
    which.className = "vh";
    which.textContent = `: ${article.getAttribute("aria-label")}`;
    button.append(label, which);
    head.append(button);
    show(li, button, start.has(article.id.slice(2)));
    button.addEventListener("click", () => {
      show(li, button, !li.classList.contains("folded"));
      toFragment();
    });
  }
})();
