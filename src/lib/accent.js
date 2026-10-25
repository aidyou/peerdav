import { ref } from "vue";

/**
 * Accent (primary colour) selection. The stored value is the user's chosen
 * hue; the actual palette is expressed in CSS via `data-accent` on <html> so
 * Element Plus and the app tokens can re-derive from one source of truth.
 */
export const DEFAULT_ACCENT = "blue";
export const SUPPORTED_ACCENTS = Object.freeze([
  "blue",
  "red",
  "purple",
  "green",
  "yellow",
]);
export const ACCENT_STORAGE_KEY = "peerdav.accent";

export const accent = ref(DEFAULT_ACCENT);

export function normalizeAccent(value) {
  return SUPPORTED_ACCENTS.includes(value) ? value : null;
}

function safeStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function readStoredAccent(storage) {
  let stored = null;
  try {
    stored = storage?.getItem(ACCENT_STORAGE_KEY) ?? null;
  } catch {
    stored = null;
  }
  return normalizeAccent(stored) ?? DEFAULT_ACCENT;
}

function applyDocumentAccent(next) {
  if (typeof document === "undefined" || !document.documentElement) return;
  document.documentElement.dataset.accent = next;
}

function persistAccent(storage, next) {
  try {
    storage?.setItem(ACCENT_STORAGE_KEY, next);
    return true;
  } catch {
    // Disabled or full storage must not break the in-memory switch.
    return false;
  }
}

/**
 * Restores the persisted accent and reflects it on the document. Safe to call
 * again after storage changes.
 */
export function initAccent(storage = safeStorage()) {
  const next = readStoredAccent(storage);
  accent.value = next;
  applyDocumentAccent(next);
  return next;
}

/**
 * Switches and persists the accent. Invalid values are ignored.
 */
export function setAccent(next, storage = safeStorage()) {
  const normalized = normalizeAccent(next);
  if (!normalized) return false;
  accent.value = normalized;
  applyDocumentAccent(normalized);
  persistAccent(storage, normalized);
  return true;
}

initAccent();
