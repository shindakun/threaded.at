// Handle lookahead: a WAI-ARIA combobox with a list popup. The field works as
// a plain input without this script.
(() => {
  const wrap = document.querySelector("[data-typeahead]");
  if (!wrap) return;
  const input = wrap.querySelector("input");
  const list = document.createElement("ul");
  list.id = "identifier-suggestions";
  list.className = "suggest";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", "Suggested accounts");
  list.hidden = true;
  const status = document.createElement("p");
  status.className = "vh";
  status.setAttribute("role", "status");
  wrap.append(list, status);
  input.setAttribute("role", "combobox");
  input.setAttribute("aria-autocomplete", "list");
  input.setAttribute("aria-expanded", "false");
  input.setAttribute("aria-controls", list.id);

  let items = [];
  let active = -1;
  let timer = 0;
  let ctrl = null;
  const query = () => input.value.trim().replace(/^@/, "").toLowerCase();
  const wanted = (q) => q.length >= 2 && !q.startsWith("did:") && !q.startsWith("http") && !q.includes("/");

  const close = () => {
    list.hidden = true;
    input.setAttribute("aria-expanded", "false");
    input.removeAttribute("aria-activedescendant");
    active = -1;
  };
  const open = () => {
    list.hidden = false;
    input.setAttribute("aria-expanded", "true");
  };
  const highlight = (i) => {
    [...list.children].forEach((li, k) => li.setAttribute("aria-selected", String(k === i)));
    active = i;
    if (i >= 0) input.setAttribute("aria-activedescendant", list.children[i].id);
    else input.removeAttribute("aria-activedescendant");
  };
  const pick = (i) => {
    input.value = items[i].handle;
    status.textContent = "";
    close();
  };
  const render = (actors) => {
    items = actors;
    list.replaceChildren();
    active = -1;
    actors.forEach((a, i) => {
      const li = document.createElement("li");
      li.id = `identifier-option-${i}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", "false");
      if (a.avatar) {
        const img = document.createElement("img");
        img.src = a.avatar;
        img.alt = "";
        img.width = 20;
        img.height = 20;
        li.append(img);
      }
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = a.displayName || a.handle;
      const handle = document.createElement("span");
      handle.className = "handle";
      handle.textContent = `@${a.handle}`;
      li.append(name, " ", handle);
      li.addEventListener("mousedown", (e) => {
        e.preventDefault();
        pick(i);
      });
      list.append(li);
    });
    status.textContent = actors.length ? `${actors.length} account${actors.length === 1 ? "" : "s"} found` : "No matching accounts";
    if (actors.length) open();
    else close();
  };

  input.addEventListener("input", () => {
    clearTimeout(timer);
    const q = query();
    if (!wanted(q)) {
      if (ctrl) ctrl.abort();
      status.textContent = "";
      close();
      return;
    }
    timer = setTimeout(async () => {
      if (ctrl) ctrl.abort();
      ctrl = new AbortController();
      try {
        const r = await fetch(`/login/typeahead?q=${encodeURIComponent(q)}`, { signal: ctrl.signal });
        if (!r.ok) return close();
        const data = await r.json();
        if (query() !== q) return;
        render(data.actors || []);
      } catch (e) {
        if (e.name !== "AbortError") close();
      }
    }, 150);
  });

  input.addEventListener("keydown", (e) => {
    const shown = !list.hidden && items.length > 0;
    if (e.key === "ArrowDown") {
      if (!items.length) return;
      e.preventDefault();
      open();
      highlight(Math.min(active + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      if (!shown) return;
      e.preventDefault();
      highlight(Math.max(active - 1, 0));
    } else if (e.key === "Enter") {
      if (shown && active >= 0) {
        e.preventDefault();
        pick(active);
      }
    } else if (e.key === "Escape") {
      if (shown) {
        e.preventDefault();
        close();
      } else if (input.value) {
        e.preventDefault();
        input.value = "";
      }
    }
  });

  input.addEventListener("blur", () => setTimeout(close, 100));
})();
