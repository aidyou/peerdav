import { describe, expect, it, vi } from "vitest";
import { useUploadBatch } from "../composables/useUploadBatch";

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}
function summary(phase = "running", extra = {}) {
  return { batchId: "batch", phase, conflict: null, completedFiles: 0, completedBytes: 0, ...extra };
}
function setup(overrides = {}) {
  const adapter = { identity: ({ peerId }) => ({ peerId }), select: vi.fn(async () => summary("selected")),
    start: vi.fn(async () => summary()), status: vi.fn(async () => summary()),
    page: vi.fn(async () => ({ entries: [] })), resolve: vi.fn(async () => {}),
    cancel: vi.fn(async () => {}), release: vi.fn(async () => {}), overwriteAllowed: () => true,
    isCursorReset: () => false, isCancellation: () => false, ...overrides };
  const scope = { peerId: "peer", shareName: "Docs", remoteDir: "/original" };
  const onFailure = vi.fn();
  const onCompleted = vi.fn();
  const batch = useUploadBatch({ adapter, captureScope: () => ({ ...scope }), scopeValid: (old) => old.peerId === scope.peerId,
    canSelect: () => true, onFailure, onCompleted });
  return { batch, adapter, scope, onFailure, onCompleted };
}

describe("shared upload batch session", () => {
  it("freezes selection destination while remote browsing continues", async () => {
    const picker = deferred();
    const { batch, adapter, scope } = setup({ select: vi.fn(() => picker.promise) });
    const pending = batch.selectUpload("files");
    scope.remoteDir = "/other";
    picker.resolve(summary("selected"));
    await pending;
    expect(adapter.select).toHaveBeenCalledWith({ peerId: "peer", shareName: "Docs", remoteDir: "/original" }, "files");
    expect(adapter.start).toHaveBeenCalledWith({ peerId: "peer", batchId: "batch", conflictPolicy: "ask" });
  });
  it("releases a late picker capability without starting after teardown", async () => {
    const picker = deferred();
    const { batch, adapter } = setup({ select: () => picker.promise });
    const pending = batch.selectUpload();
    await batch.cancelUploadSession();
    picker.resolve(summary("selected"));
    await pending;
    expect(adapter.start).not.toHaveBeenCalled();
    expect(adapter.release).toHaveBeenCalledTimes(1);
    expect(batch.uploadDialogOpen.value).toBe(false);
  });
  it("ignores late pages after teardown and releases once", async () => {
    const page = deferred();
    const { batch, adapter } = setup({ page: () => page.promise });
    const pending = batch.selectUpload();
    await Promise.resolve();
    await batch.cancelUploadSession();
    page.resolve({ entries: [{ name: "stale" }] });
    await pending;
    await batch.cancelUploadSession();
    expect(batch.uploadPage.value).toBeNull();
    expect(adapter.start).not.toHaveBeenCalled();
    expect(adapter.release).toHaveBeenCalledTimes(1);
  });
  it("replaces pages rather than appending upload history", async () => {
    const { batch, adapter } = setup();
    adapter.page.mockResolvedValueOnce({ entries: [{ name: "first" }], nextCursor: "next" })
      .mockResolvedValueOnce({ entries: [{ name: "second" }] });
    await batch.selectUpload();
    await batch.loadUploadPage("root", "next");
    expect(batch.uploadPage.value.entries).toEqual([{ name: "second" }]);
  });
  it("does not let a running poll overwrite the joined cancellation", async () => {
    const stale = deferred();
    const { batch, adapter } = setup();
    await batch.selectUpload();
    adapter.status.mockReturnValueOnce(stale.promise).mockResolvedValueOnce(summary("cancelled"));
    const polling = batch.pollUpload();
    const stopping = batch.cancelUpload();
    stale.resolve(summary("running"));
    await Promise.all([polling, stopping]);
    expect(batch.uploadSummary.value.phase).toBe("cancelled");
  });
  it("does not re-emit backend terminal errors", async () => {
    const { batch, onFailure } = setup({ start: async () => summary("failed", { error: { code: "FAILED", message: "original" } }) });
    await batch.selectUpload();
    expect(batch.uploadBatchError.value).toBe("original");
    expect(onFailure).not.toHaveBeenCalled();
  });
  it("uses adapter overwrite safety rather than relaxing cloud ETags", async () => {
    const conflict = { conflictId: "conflict", etag: null };
    const { batch, adapter } = setup({ overwriteAllowed: (item) => Boolean(item.etag), start: async () => summary("conflict", { conflict }) });
    await batch.selectUpload();
    await batch.resolveUploadConflict("overwrite");
    expect(adapter.resolve).not.toHaveBeenCalled();
    expect(batch.uploadOverwriteAllowed.value).toBe(false);
  });
});
