// Pills inside the action-required <summary> are anchor links to the
// matching signal section in the expanded panel. The browser would
// otherwise toggle <details> on click (because <summary>) AND navigate to
// the hash, which fights itself when the panel is already open. Take over:
// open the details (idempotent), prevent the toggle, smooth-scroll.
document.addEventListener("click", (event) => {
  const pill = event.target.closest("[data-action-pill]");
  if (!pill) return;
  event.preventDefault();
  event.stopPropagation();
  const details = pill.closest("details");
  if (details && !details.open) details.open = true;
  const href = pill.getAttribute("href") || "";
  const target = href.startsWith("#") ? document.getElementById(href.slice(1)) : null;
  if (target) {
    target.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }
});
