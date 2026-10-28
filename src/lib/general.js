import { reactive } from "vue";

const defaults = { autoLock: false, idleMinutes: 5, toggleShortcut: "CommandOrControl+Shift+Space", lockShortcut: "CommandOrControl+Shift+L" };
let saved = {};
try {
  const value = JSON.parse(localStorage.getItem("peerdav.general") || "{}");
  saved = value && typeof value === "object" && !Array.isArray(value) ? value : {};
} catch { /* Use safe defaults when storage is unavailable. */ }
export const generalSettings = reactive({
  autoUpdate: saved.autoUpdate !== false,
  autoLock: saved.autoLock === true,
  idleMinutes: Number.isFinite(saved.idleMinutes) ? Math.min(1440, Math.max(5, Math.ceil(saved.idleMinutes / 5) * 5)) : defaults.idleMinutes,
  toggleShortcut: typeof saved.toggleShortcut === "string" ? saved.toggleShortcut : defaults.toggleShortcut,
  lockShortcut: typeof saved.lockShortcut === "string" ? saved.lockShortcut : defaults.lockShortcut,
});
export function shortcutFromEvent(event) {
  if (event.repeat || event.isComposing || ["Control", "Meta", "Alt", "Shift", "Tab", "Escape"].includes(event.key)) return null;
  const modifiers = [];
  if (event.ctrlKey || event.metaKey) modifiers.push("CommandOrControl");
  if (event.altKey) modifiers.push("Alt");
  if (event.shiftKey) modifiers.push("Shift");
  let key = event.code || event.key;
  if (/^Key[A-Z]$/.test(key)) key = key.slice(3);
  else if (/^Digit[0-9]$/.test(key)) key = key.slice(5);
  else if (key === " ") key = "Space";
  if (!modifiers.length && !/^F([1-9]|1[0-2])$/.test(key)) return null;
  if (!/^(?:[A-Z0-9]|F(?:[1-9]|1[0-2])|Space|Enter|Backspace|Delete|Arrow(?:Up|Down|Left|Right)|Home|End|PageUp|PageDown|Backquote|Minus|Equal|BracketLeft|BracketRight|Backslash|Semicolon|Quote|Comma|Period|Slash)$/.test(key)) return null;
  return [...modifiers, key].join("+");
}

export function saveGeneralSettings() {
  generalSettings.idleMinutes = Number.isFinite(generalSettings.idleMinutes)
    ? Math.min(1440, Math.max(5, Math.ceil(generalSettings.idleMinutes / 5) * 5)) : defaults.idleMinutes;
  localStorage.setItem("peerdav.general", JSON.stringify(generalSettings));
}
