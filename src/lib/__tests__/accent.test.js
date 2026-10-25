import { beforeEach, describe, expect, it } from "vitest";

import {
  ACCENT_STORAGE_KEY,
  DEFAULT_ACCENT,
  SUPPORTED_ACCENTS,
  accent,
  initAccent,
  normalizeAccent,
  setAccent,
} from "../accent.js";

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

describe("normalizeAccent", () => {
  it("accepts every supported accent", () => {
    for (const value of SUPPORTED_ACCENTS) {
      expect(normalizeAccent(value)).toBe(value);
    }
  });

  it("rejects unsupported or empty values", () => {
    expect(normalizeAccent("teal")).toBeNull();
    expect(normalizeAccent(undefined)).toBeNull();
    expect(normalizeAccent(null)).toBeNull();
  });
});

describe("accent state", () => {
  beforeEach(() => {
    localStorage.clear();
    initAccent();
  });

  it("defaults to blue and reflects it on the document", () => {
    expect(initAccent()).toBe(DEFAULT_ACCENT);
    expect(accent.value).toBe("blue");
    expect(document.documentElement.dataset.accent).toBe("blue");
  });

  it("restores a stored accent", () => {
    localStorage.setItem(ACCENT_STORAGE_KEY, "green");
    expect(initAccent()).toBe("green");
    expect(accent.value).toBe("green");
    expect(document.documentElement.dataset.accent).toBe("green");
  });

  it("ignores an unsupported stored accent", () => {
    localStorage.setItem(ACCENT_STORAGE_KEY, "teal");
    expect(initAccent()).toBe(DEFAULT_ACCENT);
  });

  it("persists the switch and updates the document", () => {
    expect(setAccent("purple")).toBe(true);
    expect(accent.value).toBe("purple");
    expect(localStorage.getItem(ACCENT_STORAGE_KEY)).toBe("purple");
    expect(document.documentElement.dataset.accent).toBe("purple");
  });

  it("ignores unsupported switch targets", () => {
    expect(setAccent("teal")).toBe(false);
    expect(accent.value).toBe(DEFAULT_ACCENT);
    expect(localStorage.getItem(ACCENT_STORAGE_KEY)).toBeNull();
  });

  it("keeps the in-memory switch when storage is unavailable", () => {
    const storage = throwingStorage();
    expect(initAccent(storage)).toBe(DEFAULT_ACCENT);
    expect(setAccent("red", storage)).toBe(true);
    expect(accent.value).toBe("red");
    expect(document.documentElement.dataset.accent).toBe("red");
  });
});
