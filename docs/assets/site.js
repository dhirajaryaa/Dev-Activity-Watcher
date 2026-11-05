// atw documentation interactive behaviors
document.addEventListener("DOMContentLoaded", () => {
  // 1. Entrance observer
  const entrances = document.querySelectorAll(".entrance");
  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            entry.target.classList.add("is-visible");
            observer.unobserve(entry.target);
          }
        });
      },
      { rootMargin: "600px 0px 600px 0px", threshold: 0 }
    );
    entrances.forEach((el) => observer.observe(el));
  } else {
    entrances.forEach((el) => el.classList.add("is-visible"));
  }

  // 2. Showcase row interactive simulation
  const hoverRow = document.getElementById("demo-hover-row");
  const delBtn = document.getElementById("demo-del-btn");
  const rowStatus = document.getElementById("demo-row-status");
  if (hoverRow && delBtn && rowStatus) {
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (delBtn.textContent.trim() === "Delete") {
        delBtn.textContent = "Confirm";
        delBtn.style.background = "#e01a4f";
        delBtn.style.color = "#ffffff";
        rowStatus.textContent = "Pending deletion approval";
      } else {
        hoverRow.style.opacity = "0.35";
        hoverRow.style.pointerEvents = "none";
        rowStatus.textContent = "Purged from local SQLite and cloud aggregates";
        delBtn.textContent = "Purged";
        delBtn.style.background = "#111111";
      }
    });
  }

  // 3. Features: Live Controls & Sticky Preview
  const displayFormatChips = document.querySelectorAll("[data-chip-format]");
  const syncIntervalChips = document.querySelectorAll("[data-chip-sync]");
  const categoryChips = document.querySelectorAll("[data-chip-cat]");
  const barPreview = document.getElementById("bar-chip-text");
  const syncPreview = document.getElementById("preview-sync-val");
  const catPreview = document.getElementById("preview-cat-val");

  let currentFormat = "hours";
  let currentSync = "600s";
  let currentCat = "developer";

  function updatePreview() {
    if (barPreview) {
      if (currentFormat === "icon") {
        barPreview.textContent = "󱚺";
      } else if (currentFormat === "hours") {
        barPreview.textContent = "󱚺 4h";
      } else {
        barPreview.textContent = "󱚺 4h 18m";
      }
    }
    if (syncPreview) {
      syncPreview.textContent = `Interval: ${currentSync}`;
    }
    if (catPreview) {
      catPreview.textContent = `Tracking: ${currentCat} allowlist`;
    }
  }

  displayFormatChips.forEach((chip) => {
    chip.addEventListener("click", () => {
      displayFormatChips.forEach((c) => c.classList.remove("selected"));
      chip.classList.add("selected");
      currentFormat = chip.getAttribute("data-chip-format");
      updatePreview();
    });
  });

  syncIntervalChips.forEach((chip) => {
    chip.addEventListener("click", () => {
      syncIntervalChips.forEach((c) => c.classList.remove("selected"));
      chip.classList.add("selected");
      currentSync = chip.getAttribute("data-chip-sync");
      updatePreview();
    });
  });

  categoryChips.forEach((chip) => {
    chip.addEventListener("click", () => {
      categoryChips.forEach((c) => c.classList.remove("selected"));
      chip.classList.add("selected");
      currentCat = chip.getAttribute("data-chip-cat");
      updatePreview();
    });
  });

  // 4. Copy code buttons
  const copyButtons = document.querySelectorAll(".copy-btn");
  copyButtons.forEach((btn) => {
    btn.addEventListener("click", () => {
      const targetId = btn.getAttribute("data-copy-target");
      const targetEl = document.getElementById(targetId);
      if (targetEl) {
        const text = targetEl.innerText || targetEl.textContent;
        navigator.clipboard.writeText(text).then(() => {
          const original = btn.textContent;
          btn.textContent = "Copied";
          setTimeout(() => {
            btn.textContent = original;
          }, 1500);
        });
      }
    });
  });

  // 5. Query Tabs
  const tabBtns = document.querySelectorAll(".tab-btn");
  const tabPanes = document.querySelectorAll(".tab-pane");
  tabBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      tabBtns.forEach((b) => b.classList.remove("active"));
      tabPanes.forEach((p) => p.classList.remove("active"));
      btn.classList.add("active");
      const tabId = btn.getAttribute("data-tab");
      const activePane = document.getElementById(tabId);
      if (activePane) {
        activePane.classList.add("active");
      }
    });
  });

  // 6. FAQ Question index -> answer crossfade
  const faqItems = document.querySelectorAll(".faq-q-item");
  const faqAnswerText = document.getElementById("faq-current-answer");
  const answers = {
    faq1: "Tracking is local by default in SQLite (~/.local/share/atw/activity.db). Cloud sync is strictly optional and targets your own self-hosted or managed Turso database.",
    faq2: "Window titles, file paths, keystrokes, and git branches are never captured or sent. Only the application binary class, high-level category, and UTC start/end timestamps are recorded.",
    faq3: "The daemon checks your machine using Wayland's ext-idle-notify-v1 protocol (default 300 seconds) and tracks suspend through clock drift, discarding idle intervals immediately.",
    faq4: "atw uses interval union math. When two devices run at the same time, overlapping segments are merged once; overlap across different apps is attributed to an (overlapping) category without double-counting.",
    faq5: "Run 'atw delete-app <AppName>' or click the delete button in the panel. The application's window classes are appended to excluded_apps in config.toml, and cloud records are purged in one transaction."
  };

  faqItems.forEach((item) => {
    item.addEventListener("click", () => {
      faqItems.forEach((i) => i.classList.remove("active"));
      item.classList.add("active");
      const qKey = item.getAttribute("data-faq");
      if (faqAnswerText && answers[qKey]) {
        faqAnswerText.style.opacity = "0.2";
        setTimeout(() => {
          faqAnswerText.textContent = answers[qKey];
          faqAnswerText.style.opacity = "1";
        }, 120);
      }
    });
  });
});
