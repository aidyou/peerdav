import { beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => { localStorage.clear(); vi.resetModules(); });
describe("general preferences", () => {
  it("persists lock preferences and clamps idle time", async () => {
    const { generalSettings, saveGeneralSettings } = await import("../general");
    generalSettings.autoLock = true;
    generalSettings.idleMinutes = 0;
    saveGeneralSettings();
    expect(generalSettings.idleMinutes).toBe(5);
    expect(JSON.parse(localStorage.getItem("peerdav.general")).autoLock).toBe(true);
    vi.resetModules();
    const restored = await import("../general");
    expect(restored.generalSettings.autoLock).toBe(true);
    expect(restored.generalSettings.idleMinutes).toBe(5);
  });
  it("normalizes old durations to five-minute increments", async () => {
    localStorage.setItem("peerdav.general", JSON.stringify({ idleMinutes: 7 }));
    const { generalSettings, saveGeneralSettings } = await import("../general");
    expect(generalSettings.idleMinutes).toBe(10);
    generalSettings.idleMinutes = 1445;
    saveGeneralSettings();
    expect(generalSettings.idleMinutes).toBe(1440);
  });
  it("captures physical keys and ignores incomplete or unmodified shortcuts", async () => {
    const { shortcutFromEvent } = await import("../general");
    expect(shortcutFromEvent({ key: "≈", code: "KeyX", metaKey: true, altKey: true })).toBe("CommandOrControl+Alt+X");
    expect(shortcutFromEvent({ key: " ", code: "Space", ctrlKey: true, shiftKey: true })).toBe("CommandOrControl+Shift+Space");
    expect(shortcutFromEvent({ key: "F8", code: "F8" })).toBe("F8");
    for (const key of ["Control", "Escape", "Tab", "x"]) expect(shortcutFromEvent({ key, code: key })).toBeNull();
  });
  it("uses safe defaults for malformed storage", async () => {
    localStorage.setItem("peerdav.general", "not json");
    const { generalSettings } = await import("../general");
    expect(generalSettings.autoLock).toBe(false);
    expect(generalSettings.idleMinutes).toBe(5);
  });
});
