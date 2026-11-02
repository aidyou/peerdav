import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("../invoke", () => ({ invoke: vi.fn(async () => null) }));
import { invoke } from "../invoke";
import { peerUploadBatch } from "../peer";
import { cloudUploadAdapter, peerUploadAdapter } from "../uploadBatchAdapters";

beforeEach(() => invoke.mockClear());
describe("peer batch IPC adapter", () => {
  it("sends native selection scope and options without local paths", async () => {
    await peerUploadBatch.select({ peerId: "peer", shareName: "Docs", remoteDir: "/sub", mode: "files",
      options: { respectGitignore: true, patterns: "*.tmp" }, localPath: "/private" });
    expect(invoke).toHaveBeenCalledWith("peer_select_upload_batch", { peerId: "peer", shareName: "Docs", remoteDir: "/sub", mode: "files",
      options: { respectGitignore: true, patterns: "*.tmp" } });
  });
  it("uses scoped opaque capabilities for every batch operation", async () => {
    const args = { peerId: "peer", batchId: "batch", localPath: "/private" };
    await peerUploadBatch.start(args);
    await peerUploadBatch.status(args);
    await peerUploadBatch.page(args);
    await peerUploadBatch.resolve({ ...args, conflictId: "conflict", decision: "skip" });
    await peerUploadBatch.cancel(args);
    await peerUploadBatch.release(args);
    expect(invoke.mock.calls).toEqual([
      ["peer_start_upload_batch", { peerId: "peer", batchId: "batch", conflictPolicy: "ask" }],
      ["peer_status_upload_batch", { peerId: "peer", batchId: "batch" }],
      ["peer_page_upload_batch", { peerId: "peer", batchId: "batch", cursor: null, limit: 100 }],
      ["peer_resolve_upload_batch", { peerId: "peer", batchId: "batch", conflictId: "conflict", decision: "skip", applyToRemaining: false }],
      ["peer_cancel_upload_batch", { peerId: "peer", batchId: "batch" }],
      ["peer_release_upload_batch", { peerId: "peer", batchId: "batch" }],
    ]);
  });
  it("allows Peer overwrite without weakening Cloud ETag safety", () => {
    expect(peerUploadAdapter.overwriteAllowed({ etag: null })).toBe(true);
    expect(cloudUploadAdapter.overwriteAllowed({ etag: null })).toBe(false);
    expect(cloudUploadAdapter.overwriteAllowed({ etag: "revision" })).toBe(true);
  });
});
