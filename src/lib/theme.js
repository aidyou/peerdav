import { computed, ref } from "vue";

/**
 * Appearance theme state. The stored value is the user preference; the
 * rendered theme is derived from the preference plus the live OS signal so a
 * "system" choice follows the OS in real time. Exactly one theme is applied at
 * a time and the choice is persisted across launches.
 */
export const DEFAULT_THEME = "system";
export const SUPPORTED_THEMES = Object.freeze(["system", "light", "dark"]);
export const THEME_STORAGE_KEY = "peerdav.theme";

/** User preference: one of SUPPORTED_THEMES. */
export const theme = ref(DEFAULT_THEME);

/** Live "does the OS want dark?" signal, used only when the preference is "system". */
export const systemPrefersDark = ref(false);

export function normalizeTheme(value) {
  return SUPPORTED_THEMES.includes(value) ? value : null;
}

/**
 * Pure mapping from a preference and the system signal to the effective theme.
 * Kept side-effect free so it can be unit tested without a DOM.
 */
export function resolveTheme(preference, prefersDark) {
  if (preference === "light") return "light";
  if (preference === "dark") return "dark";
  return prefersDark ? "dark" : "light";
}

export const resolvedTheme = computed(() =>
  resolveTheme(theme.value, systemPrefersDark.value),
);

function safeStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function readStoredTheme(storage) {
  let stored = null;
  try {
    stored = storage?.getItem(THEME_STORAGE_KEY) ?? null;
  } catch {
    stored = null;
  }
  return normalizeTheme(stored) ?? DEFAULT_THEME;
}

// jsdom and some webviews do not implement matchMedia; fall back to "light" so
// the system preference never throws during tests or on unsupported hosts.
function readSystemPrefersDark() {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  try {
    return window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch {
    return false;
  }
}

// Element Plus dark mode keys off the `dark` class on <html>; the data-theme
// attribute drives the app's own semantic tokens.
function applyDocumentTheme(resolved) {
  if (typeof document === "undefined" || !document.documentElement) return;
  document.documentElement.dataset.theme = resolved;
  document.documentElement.classList.toggle("dark", resolved === "dark");
}

let stopSystemWatcher = null;

function watchSystemTheme() {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return;
  }
  if (stopSystemWatcher) return;
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = (event) => {
    systemPrefersDark.value = event.matches;
    applyDocumentTheme(resolvedTheme.value);
  };
  if (typeof query.addEventListener === "function") {
    query.addEventListener("change", onChange);
    stopSystemWatcher = () => query.removeEventListener("change", onChange);
  } else if (typeof query.addListener === "function") {
    // Older Safari/WebKitGTK matchMedia API.
    query.addListener(onChange);
    stopSystemWatcher = () => query.removeListener(onChange);
  }
}

function persistTheme(storage, next) {
  try {
    storage?.setItem(THEME_STORAGE_KEY, next);
    return true;
  } catch {
    // Disabled or full storage must not break the in-memory switch.
    return false;
  }
}

/**
 * Restores the persisted theme, reads the current OS signal and starts tracking
 * system changes. Safe to call again after storage changes.
 */
export function initTheme(storage = safeStorage()) {
  systemPrefersDark.value = readSystemPrefersDark();
  const next = readStoredTheme(storage);
  theme.value = next;
  applyDocumentTheme(resolvedTheme.value);
  watchSystemTheme();
  return next;
}

/**
 * Switches and persists the theme preference. Invalid values are ignored.
 */
export function setTheme(next, storage = safeStorage()) {
  const normalized = normalizeTheme(next);
  if (!normalized) return false;
  theme.value = normalized;
  applyDocumentTheme(resolvedTheme.value);
  persistTheme(storage, normalized);
  return true;
}

initTheme();
