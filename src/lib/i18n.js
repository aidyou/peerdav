import { ref } from "vue";

import en from "../locales/en.js";
import zhCN from "../locales/zh-CN.js";

/**
 * UI locale state. Exactly one language is rendered at a time; the choice is
 * persisted and restored on the next launch.
 */
export const DEFAULT_LOCALE = "zh-CN";
export const SUPPORTED_LOCALES = Object.freeze(["zh-CN", "en"]);
export const LOCALE_STORAGE_KEY = "peerdav.locale";

/**
 * Backend user-facing messages are built as "中文 / English". The UI shows the
 * half that belongs to the active locale.
 */
const BILINGUAL_SEPARATOR = " / ";

const CATALOGS = Object.freeze({
  "zh-CN": zhCN,
  en,
});

export const locale = ref(DEFAULT_LOCALE);

function safeStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function normalizeLocale(value) {
  return SUPPORTED_LOCALES.includes(value) ? value : null;
}

/**
 * Picks a locale from the host environment when the user has not chosen one.
 * Any Chinese tag (simplified or traditional) maps to the single Chinese
 * catalogue; every other language falls back to English.
 */
export function detectSystemLocale() {
  const nav = globalThis.navigator;
  const tags =
    nav && Array.isArray(nav.languages) && nav.languages.length
      ? nav.languages
      : [nav?.language];
  for (const tag of tags) {
    if (typeof tag === "string" && tag.toLowerCase().startsWith("zh")) {
      return "zh-CN";
    }
  }
  return "en";
}

function readStoredLocale(storage) {
  let stored = null;
  try {
    stored = storage?.getItem(LOCALE_STORAGE_KEY) ?? null;
  } catch {
    stored = null;
  }
  return normalizeLocale(stored) ?? detectSystemLocale();
}

function applyDocumentLang(next) {
  if (typeof document !== "undefined" && document.documentElement) {
    document.documentElement.lang = next;
  }
}

function persistLocale(storage, next) {
  try {
    storage?.setItem(LOCALE_STORAGE_KEY, next);
    return true;
  } catch {
    // Disabled or full storage must not break the in-memory switch.
    return false;
  }
}

/**
 * Restores the persisted locale. Safe to call again after storage changes.
 */
export function initLocale(storage = safeStorage()) {
  const next = readStoredLocale(storage);
  locale.value = next;
  applyDocumentLang(next);
  return next;
}

/**
 * Switches and persists the locale. Invalid values are ignored.
 */
export function setLocale(next, storage = safeStorage()) {
  const normalized = normalizeLocale(next);
  if (!normalized) return false;
  locale.value = normalized;
  applyDocumentLang(normalized);
  persistLocale(storage, normalized);
  return true;
}

function lookup(key, target) {
  const catalog = CATALOGS[target];
  if (catalog && Object.prototype.hasOwnProperty.call(catalog, key)) {
    return catalog[key];
  }
  const fallback = CATALOGS[DEFAULT_LOCALE];
  return Object.prototype.hasOwnProperty.call(fallback, key) ? fallback[key] : key;
}

function interpolate(template, params) {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name) =>
    Object.prototype.hasOwnProperty.call(params, name)
      ? String(params[name])
      : match,
  );
}

/**
 * Translates one key for the active locale. Missing keys fall back to the
 * default catalogue so the UI never shows a raw key.
 */
export function t(key, params) {
  return interpolate(lookup(key, locale.value), params);
}

/**
 * Picks the active-language half of a backend "中文 / English" message.
 * Messages without the separator are returned unchanged.
 */
export function localizeMessage(message) {
  if (typeof message !== "string" || !message) return "";
  const index = message.indexOf(BILINGUAL_SEPARATOR);
  if (index < 0) return message;

  const chinese = message.slice(0, index).trim();
  const english = message.slice(index + BILINGUAL_SEPARATOR.length).trim();
  if (!chinese) return english || message;
  if (!english) return chinese;

  return locale.value === DEFAULT_LOCALE ? chinese : english;
}

initLocale();