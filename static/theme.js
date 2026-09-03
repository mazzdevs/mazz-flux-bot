// Shared light/dark theme toggle, used by both the dashboard and the
// project detail page. The actual theme attribute is set synchronously in
// an inline <head> script (see index.html/project.html) to avoid a flash of
// the wrong theme on load — this file only wires up the toggle button.

(function () {
  const THEME_KEY = "mfb-theme";

  function currentTheme() {
    const attr = document.documentElement.getAttribute("data-theme");
    if (attr) return attr;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }

  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem(THEME_KEY, theme);
    const icon = document.getElementById("theme-icon");
    if (icon) {
      icon.innerHTML = `<use href="/icons.svg#${theme === "dark" ? "icon-moon" : "icon-sun"}"></use>`;
    }
  }

  document.addEventListener("DOMContentLoaded", () => {
    applyTheme(currentTheme());
    const toggle = document.getElementById("theme-toggle");
    if (toggle) {
      toggle.addEventListener("click", () => {
        applyTheme(currentTheme() === "dark" ? "light" : "dark");
      });
    }
  });
})();
