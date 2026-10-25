import { beforeEach, describe, expect, it } from "vitest";

import en from "../../locales/en.js";
import zhCN from "../../locales/zh-CN.js";
import {
  DEFAULT_LOCALE,
  LOCALE_STORAGE_KEY,
  initLocale,
  locale,
  localizeMessage,
  normalizeLocale,
  setLocale,
  t,
} from "../i18n.js";
import {
  errorStrategy,
  normalizeAppError,
  toUserMessage,
} from "../invoke.js";

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

describe("locale state", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
  });

  it("defaults to simplified Chinese with no stored value", () => {
    expect(initLocale()).toBe(DEFAULT_LOCALE);
    expect(locale.value).toBe("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("restores a stored locale on start", () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "en");
    expect(initLocale()).toBe("en");
    expect(locale.value).toBe("en");
    expect(document.documentElement.lang).toBe("en");
  });

  it("ignores an unsupported stored locale", () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "fr");
    expect(initLocale()).toBe(DEFAULT_LOCALE);
  });

  it("persists the switch and updates the document language", () => {
    expect(setLocale("en")).toBe(true);
    expect(locale.value).toBe("en");
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe("en");
    expect(document.documentElement.lang).toBe("en");

    expect(setLocale("zh-CN")).toBe(true);
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("ignores unsupported switch targets", () => {
    expect(setLocale("fr")).toBe(false);
    expect(locale.value).toBe(DEFAULT_LOCALE);
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBeNull();
  });

  it("keeps the in-memory switch when storage is unavailable", () => {
    const storage = throwingStorage();
    expect(initLocale(storage)).toBe(DEFAULT_LOCALE);
    expect(setLocale("en", storage)).toBe(true);
    expect(locale.value).toBe("en");
  });

  it("normalizes only supported locales", () => {
    expect(normalizeLocale("en")).toBe("en");
    expect(normalizeLocale("zh-CN")).toBe("zh-CN");
    expect(normalizeLocale("en-US")).toBeNull();
    expect(normalizeLocale(undefined)).toBeNull();
  });
});

describe("catalogues", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
  });

  it("keeps the English and Chinese key sets identical", () => {
    expect(Object.keys(en).sort()).toEqual(Object.keys(zhCN).sort());
  });

  it("renders one language at a time", () => {
    expect(t("vault.initializeTitle")).toBe("首次初始化");
    setLocale("en");
    expect(t("vault.initializeTitle")).toBe("First-time setup");
  });

  it("interpolates parameters", () => {
    expect(t("pending.attempts", { count: 3 })).toBe("3 次尝试");
    setLocale("en");
    expect(t("pending.attempts", { count: 3 })).toBe("3 attempts");
  });

  it("returns the key itself for unknown keys", () => {
    expect(t("missing.key")).toBe("missing.key");
  });
});

describe("bilingual backend messages", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
  });

  it("shows the Chinese half in the Chinese locale", () => {
    expect(
      localizeMessage(
        "保险箱已锁定，请先解锁 / The vault is locked. Unlock it first.",
      ),
    ).toBe("保险箱已锁定，请先解锁");
  });

  it("shows the English half in the English locale", () => {
    setLocale("en");
    expect(
      localizeMessage(
        "保险箱已锁定，请先解锁 / The vault is locked. Unlock it first.",
      ),
    ).toBe("The vault is locked. Unlock it first.");
  });

  it("passes through messages without a separator", () => {
    expect(localizeMessage("plain message")).toBe("plain message");
    expect(localizeMessage("")).toBe("");
    expect(localizeMessage(null)).toBe("");
  });
});

describe("error mapping", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
  });

  it("maps stable error codes to UI strategies", () => {
    const error = (code) => ({ code, message: "message" });
    expect(errorStrategy(error("VAULT_LOCKED"))).toBe("unlock");
    expect(errorStrategy(error("AUTH_INVALID_CREDENTIALS"))).toBe("retry");
    expect(errorStrategy(error("PEER_NOT_FOUND"))).toBe("peer-refresh");
    expect(errorStrategy(error("SERVER_BIND_FAILED"))).toBe("edit");
    expect(errorStrategy(error("SOMETHING_NEW"))).toBe("show");
  });

  it("normalizes unknown rejections into a structured error", () => {
    const normalized = normalizeAppError("boom");
    expect(normalized.code).toBe("UNKNOWN_ERROR");
    expect(normalized.detail).toBe("boom");
    expect(normalized.context).toBeNull();
  });

  it("localizes the user message without touching detail", () => {
    const error = {
      code: "VAULT_LOCKED",
      message: "保险箱已锁定，请先解锁 / The vault is locked. Unlock it first.",
      detail: "raw cause",
      context: { operation: "unlock" },
    };
    expect(toUserMessage(error)).toBe("保险箱已锁定，请先解锁");
    setLocale("en");
    expect(toUserMessage(error)).toBe("The vault is locked. Unlock it first.");
    expect(normalizeAppError(error).detail).toBe("raw cause");
  });
});