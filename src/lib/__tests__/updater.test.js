import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../invoke", () => ({ invoke: vi.fn() }));
import { invoke } from "../invoke";
import { applyUpdateStatus, checkForUpdates, updateBusy, updateProgress, updateState } from "../updater";

beforeEach(() => {
  invoke.mockReset();
  applyUpdateStatus({ configured: true, phase: "idle", currentVersion: "0.1.0", downloaded: 0, total: null, version: null, notes: null });
});

afterEach(() => { vi.useRealTimers(); });

describe("updater state", () => {
  it("silently retries automatic failures after 8 and 16 seconds, then stops", async () => {
    vi.useFakeTimers();
    invoke.mockRejectedValue({ code: "UPDATE_FAILED", message: "offline" });
    const check = checkForUpdates({ automatic: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(invoke).toHaveBeenCalledTimes(1);
    await checkForUpdates();
    expect(invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(7999);
    expect(invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(15999);
    expect(invoke).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    await expect(check).resolves.toBeUndefined();
    expect(invoke).toHaveBeenCalledTimes(3);
    expect(invoke).toHaveBeenLastCalledWith("check_for_updates", { automatic: true });
    expect(updateState.phase).not.toBe("failed");
    await vi.advanceTimersByTimeAsync(60000);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("stops retrying after a successful automatic check", async () => {
    vi.useFakeTimers();
    invoke.mockRejectedValueOnce({ code: "UPDATE_FAILED" }).mockResolvedValueOnce({ phase: "upToDate" });
    const check = checkForUpdates({ automatic: true });
    await vi.advanceTimersByTimeAsync(8000);
    await check;
    await vi.advanceTimersByTimeAsync(16000);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(updateState.phase).toBe("upToDate");
  });

  it("cancels retry waits on unmount and respects disabling automatic updates", async () => {
    vi.useFakeTimers();
    invoke.mockRejectedValue({ code: "UPDATE_FAILED" });
    const controller = new AbortController();
    const check = checkForUpdates({ automatic: true, signal: controller.signal });
    await vi.advanceTimersByTimeAsync(0);
    controller.abort();
    await check;
    expect(vi.getTimerCount()).toBe(0);
    let enabled = true;
    const next = checkForUpdates({ automatic: true, enabled: () => enabled });
    await vi.advanceTimersByTimeAsync(0);
    enabled = false;
    await vi.advanceTimersByTimeAsync(8000);
    await next;
    expect(invoke).toHaveBeenCalledTimes(2);
  });
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
