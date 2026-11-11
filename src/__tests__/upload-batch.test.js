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
  it('asks again unchecked for a second conflict and resets a new queue to Ask', async () => {
    const first = { conflictId: 'first', relativePath: 'a.txt' };
    const second = { conflictId: 'second', relativePath: 'b.txt' };
    const { batch, adapter } = setup({ start: vi.fn(async () => summary('conflict', { conflict: first })) });
    await batch.selectUpload('single');
    expect(batch.uploadApplyRemaining.value).toBe(false);
    adapter.status.mockResolvedValueOnce(summary('conflict', { conflict: second, skippedFiles: 1 }));
    await batch.resolveUploadConflict('skip');
    expect(adapter.resolve).toHaveBeenLastCalledWith({ peerId: 'peer', batchId: 'batch', conflictId: 'first', decision: 'skip', applyToRemaining: false });
    expect(batch.uploadApplyRemaining.value).toBe(false);
    expect(batch.uploadSummary.value.skippedFiles).toBe(1);
    batch.uploadApplyRemaining.value = true;
    adapter.status.mockResolvedValueOnce(summary('completed', { completedFiles: 1, skippedFiles: 1 }));
    await batch.resolveUploadConflict('overwrite');
    expect(adapter.resolve).toHaveBeenLastCalledWith({ peerId: 'peer', batchId: 'batch', conflictId: 'second', decision: 'overwrite', applyToRemaining: true });
    await batch.closeUpload(); await batch.selectUpload('files');
    expect(batch.uploadApplyRemaining.value).toBe(false);
    expect(adapter.start).toHaveBeenLastCalledWith({ peerId: 'peer', batchId: 'batch', conflictPolicy: 'ask' });
  });
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
    const stopping = batch.cancelUploadSession();
    picker.resolve(summary("selected"));
    await Promise.all([pending, stopping]);
    expect(adapter.start).not.toHaveBeenCalled();
    expect(adapter.release).toHaveBeenCalledTimes(1);
    expect(batch.uploadDialogOpen.value).toBe(false);
  });
  it("ignores late pages after teardown and releases once", async () => {
    const page = deferred();
    const { batch, adapter } = setup({ page: () => page.promise });
    const pending = batch.selectUpload();
    await Promise.resolve();
    const stopping = batch.cancelUploadSession();
    page.resolve({ entries: [{ name: "stale" }] });
    await Promise.all([pending, stopping]);
    await batch.cancelUploadSession();
    expect(batch.uploadPage.value).toBeNull();
    expect(adapter.start).not.toHaveBeenCalled();
    expect(adapter.release).toHaveBeenCalledTimes(1);
  });
  it('joins late start cleanup before resolving teardown', async () => {
    const start = deferred();
    const released = deferred();
    const { batch, adapter } = setup({ start: vi.fn(() => start.promise), release: vi.fn(() => released.promise) });
    const pending = batch.selectUpload();
    for (let index = 0; index < 8 && !adapter.start.mock.calls.length; index++) await Promise.resolve();
    expect(adapter.start).toHaveBeenCalledTimes(1);
    let stopped = false;
    const stopping = batch.cancelUploadSession().then(() => { stopped = true; });
    await Promise.resolve();
    expect(stopped).toBe(false);
    start.resolve(summary());
    for (let index = 0; index < 8 && !adapter.release.mock.calls.length; index++) await Promise.resolve();
    expect(adapter.release).toHaveBeenCalledTimes(1);
    expect(stopped).toBe(false);
    released.resolve();
    await Promise.all([pending, stopping]);
    expect(stopped).toBe(true);
    expect(adapter.cancel).toHaveBeenCalledTimes(2);
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
