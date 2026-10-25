import { beforeEach, describe, expect, it } from "vitest";

import {
  DEFAULT_THEME,
  SUPPORTED_THEMES,
  THEME_STORAGE_KEY,
  initTheme,
  normalizeTheme,
  resolvedTheme,
  resolveTheme,
  setTheme,
  systemPrefersDark,
  theme,
} from "../theme.js";

function throwingStorage() {
  return {
    getItem() {
      throw new Error("storage disabled");
    },
    setItem() {
      throw new Error("storage disabled");
    },
  };
}

describe("resolveTheme", () => {
  it("follows the system signal for the system preference", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
  });

  it("honors an explicit light or dark choice over the system signal", () => {
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});

describe("normalizeTheme", () => {
  it("accepts every supported theme", () => {
    for (const value of SUPPORTED_THEMES) {
      expect(normalizeTheme(value)).toBe(value);
    }
  });

  it("rejects unsupported or empty values", () => {
    expect(normalizeTheme("sepia")).toBeNull();
    expect(normalizeTheme(undefined)).toBeNull();
    expect(normalizeTheme(null)).toBeNull();
  });
});

describe("theme state", () => {
  beforeEach(() => {
    localStorage.clear();
    systemPrefersDark.value = false;
    initTheme();
  });

  it("defaults to the system preference and applies a resolved theme", () => {
    expect(initTheme()).toBe(DEFAULT_THEME);
    expect(theme.value).toBe("system");
    // jsdom has no matchMedia, so the system signal is false and resolves to light.
    expect(resolvedTheme.value).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });

  it("restores a stored theme and reflects it on the document", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "dark");
    expect(initTheme()).toBe("dark");
    expect(theme.value).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });

  it("ignores an unsupported stored theme", () => {
    localStorage.setItem(THEME_STORAGE_KEY, "sepia");
    expect(initTheme()).toBe(DEFAULT_THEME);
  });

  it("persists the switch and updates the document", () => {
    expect(setTheme("dark")).toBe(true);
    expect(theme.value).toBe("dark");
    expect(resolvedTheme.value).toBe("dark");
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);

    expect(setTheme("light")).toBe(true);
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });

  it("ignores unsupported switch targets", () => {
    expect(setTheme("sepia")).toBe(false);
    expect(theme.value).toBe(DEFAULT_THEME);
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBeNull();
  });

  it("keeps the in-memory switch when storage is unavailable", () => {
    const storage = throwingStorage();
    expect(initTheme(storage)).toBe(DEFAULT_THEME);
    expect(setTheme("dark", storage)).toBe(true);
    expect(theme.value).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("resolves to dark when following a dark system signal", () => {
    systemPrefersDark.value = true;
    theme.value = "system";
    expect(resolvedTheme.value).toBe("dark");
  });
});
