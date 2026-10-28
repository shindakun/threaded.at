// Votes in place. Without this script each vote is a form post and a reload.
document.addEventListener("submit", async (e) => {
  const form = e.target;
  if (!(form instanceof HTMLFormElement) || !form.classList.contains("vote")) return;
  e.preventDefault();
  const button = form.querySelector("button");
  const score = form.parentElement.querySelector("[data-score]");
  button.disabled = true;
  try {
    const r = await fetch(form.action, {
      method: "POST",
      headers: { Accept: "application/json" },
      body: new URLSearchParams(new FormData(form)),
    });
    if (!r.ok) {
      form.submit();
      return;
    }
    const data = await r.json();
    button.setAttribute("aria-pressed", String(data.voted));
    if (score) {
      const unit = data.score === 1 ? " point" : " points";
      score.textContent = score.hasAttribute("data-unit") ? data.score + unit : String(data.score);
    }
  } catch {
    form.submit();
  } finally {
    button.disabled = false;
  }
});

// Save and hide in place: the link's words flip ("save" and "unsave") and the
// row stays put, so focus does not jump. Without this script each is a form
// post and a reload.
document.addEventListener("submit", async (e) => {
  const form = e.target;
  if (!(form instanceof HTMLFormElement) || !form.classList.contains("mark")) return;
  e.preventDefault();
  const button = form.querySelector("button");
  const field = form.querySelector('input[name="on"]');
  const label = form.querySelector("[data-label]");
  button.disabled = true;
  try {
    const r = await fetch(form.action, {
      method: "POST",
      headers: { Accept: "application/json" },
      body: new URLSearchParams(new FormData(form)),
    });
    if (!r.ok) {
      form.submit();
      return;
    }
    const data = await r.json();
    field.value = data.on ? "0" : "1";
    label.textContent = data.on ? form.dataset.on : form.dataset.off;
  } catch {
    form.submit();
  } finally {
    button.disabled = false;
  }
});
