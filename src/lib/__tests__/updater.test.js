import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../invoke", () => ({ invoke: vi.fn() }));
import { invoke } from "../invoke";
import { applyUpdateStatus, checkForUpdates, updateBusy, updateProgress, updateState } from "../updater";

beforeEach(() => {
  invoke.mockReset();
  applyUpdateStatus({ configured: true, phase: "idle", currentVersion: "0.1.0", downloaded: 0, total: null, version: null, notes: null });
});

describe("updater state", () => {
  it("handles unknown and zero lengths without invalid percentages", () => {
    for (const total of [null, 0]) {
      applyUpdateStatus({ phase: "downloading", downloaded: 100, total });
      expect(updateProgress.value).toBe(0);
    }
    applyUpdateStatus({ phase: "downloading", total: 200 });
    expect(updateProgress.value).toBe(50);
    applyUpdateStatus({ phase: "downloading", downloaded: 300 });
    expect(updateProgress.value).toBe(99);
    expect(updateBusy.value).toBe(true);
    applyUpdateStatus({ phase: "ready" });
    expect(updateBusy.value).toBe(false);
  });

  it("deduplicates pending checks before the first progress event", async () => {
    let resolve;
    invoke.mockReturnValue(new Promise((done) => { resolve = done; }));
    const first = checkForUpdates();
    await checkForUpdates();
    expect(invoke).toHaveBeenCalledTimes(1);
    resolve({ phase: "upToDate" });
    await first;
    expect(updateState.phase).toBe("upToDate");
  });

  it("propagates failures and permits retry instead of reporting up to date", async () => {
    const error = { code: "UPDATE_FAILED", message: "network unavailable" };
    invoke.mockRejectedValueOnce(error).mockResolvedValueOnce({ phase: "ready", version: "0.2.0" });
    await expect(checkForUpdates()).rejects.toEqual(error);
    expect(updateState.phase).not.toBe("upToDate");
    await checkForUpdates();
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(updateState.version).toBe("0.2.0");
  });

  it("does not start checks during downloading or installation", async () => {
    for (const phase of ["checking", "downloading", "installing"]) {
      applyUpdateStatus({ phase });
      await checkForUpdates();
    }
    expect(invoke).not.toHaveBeenCalled();
    applyUpdateStatus(undefined);
    expect(updateState.phase).toBe("installing");
  });
});
