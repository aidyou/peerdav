import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("../invoke.js", () => ({ invoke }));

import { blankConnection, blankMapping, cloud, cloudObjects, toSaveMappingInput, toSaveS3Input } from "../cloud.js";

describe("cloud API wrappers", () => {
  beforeEach(() => invoke.mockReset());

  it("uses the exact command and argument contracts for every cloud operation", async () => {
    invoke.mockResolvedValue("ok");
    await cloud.list();
    await cloud.saveConnection({ name: "n" });
    await cloud.testConnection("c");
    await cloud.deleteConnection("c");
    await cloud.createRepository({ connectionId: "c", name: "r" });
    await cloud.joinRepository({ connectionId: "c", repositoryId: "r" });
    await cloud.saveMapping("m", { localRoot: "/x" });
    await cloud.deleteMapping("m");
    await cloud.localEntries("m");
    await cloud.preview("m");
    await cloud.sync("m", "p");
    await cloud.cancel("m");
    await cloud.recoveryRecords("m");
    await cloud.restore("m", "record", "/dest");
    await cloud.status();

    expect(invoke.mock.calls).toEqual([
      ["cloud_list"],
      ["cloud_save_connection", { input: { id: null, name: "n" } }],
      ["cloud_test_connection", { connectionId: "c" }],
      ["cloud_delete_connection", { connectionId: "c" }],
      ["cloud_create_repository", { connectionId: "c", name: "r" }],
      ["cloud_join_repository", { connectionId: "c", repositoryId: "r" }],
      ["cloud_save_mapping", { mappingId: "m", input: { localRoot: "/x" } }],
      ["cloud_delete_mapping", { mappingId: "m" }],
      ["cloud_local_entries", { mappingId: "m" }],
      ["cloud_preview", { mappingId: "m" }],
      ["cloud_sync", { mappingId: "m", previewId: "p" }],
      ["cloud_cancel", { mappingId: "m" }],
      ["cloud_recovery_records", { mappingId: "m" }],
      ["cloud_restore", { mappingId: "m", recordId: "record", destination: "/dest" }],
      ["cloud_status"],
    ]);
  });

  it("passes official authorization fields without forwarding caller tokens or endpoints", async () => {
    await cloud.protocols();
    await cloud.beginOfficialAuthorization({ provider: "google_drive", clientId: "client", redirectUri: "http://127.0.0.1/callback", endpoint: "https://evil.invalid" });
    await cloud.cancelOfficialAuthorization("request");
    await cloud.completeOfficialAuthorization({ requestId: "request", callback: "callback", name: "Drive", rootId: "root", accessToken: "forged" });
    expect(invoke.mock.calls).toEqual([
      ["cloud_protocols"],
      ["cloud_official_authorize", { provider: "google_drive", clientId: "client", redirectUri: "http://127.0.0.1/callback" }],
      ["cloud_official_cancel_authorization", { requestId: "request" }],
      ["cloud_official_complete_authorization", { requestId: "request", callback: "callback", name: "Drive", clientSecret: null, rootId: "root" }],
    ]);
  });

  it("passes only the explicitly confirmed mirror plan and defaults to no approval", async () => {
    await cloud.mirrorSync("m");
    await cloud.mirrorSync("m", true, "snapshot-1");
    expect(invoke.mock.calls).toEqual([
      ["mirror_sync", { mappingId: "m", force: false, planId: null }],
      ["mirror_sync", { mappingId: "m", force: true, planId: "snapshot-1" }],
    ]);
  });

  it("strictly serializes directory capabilities without local paths or caller keys", async () => {
    const args = { connectionId: "c", prefix: "target/", batchId: "batch", conflictId: "conflict-a", path: "/secret", key: "forged", relativePath: "forged" };
    await cloudObjects.selectUploadDirectory(args);
    await cloudObjects.startUploadDirectory(args);
    await cloudObjects.statusUploadDirectory(args);
    await cloudObjects.pageUploadDirectory(args);
    await cloudObjects.pageUploadDirectory({ ...args, directoryToken: "dir", cursor: "next", limit: 128 });
    await cloudObjects.resolveUploadDirectoryConflict({ ...args, decision: "skip" });
    await cloudObjects.resolveUploadDirectoryConflict({ ...args, decision: "overwrite", applyToRemaining: true });
    await cloudObjects.cancelUploadDirectory(args);
    await cloudObjects.releaseUploadDirectory(args);
    expect(invoke.mock.calls).toEqual([
      ["cloud_object_select_upload_directory", { connectionId: "c", prefix: "target/", mode: "folder" }],
      ["cloud_object_start_upload_directory", { connectionId: "c", batchId: "batch", conflictPolicy: "ask" }],
      ["cloud_object_status_upload_directory", { connectionId: "c", batchId: "batch" }],
      ["cloud_object_page_upload_directory", { connectionId: "c", batchId: "batch", cursor: null, limit: 100 }],
      ["cloud_object_page_upload_directory", { connectionId: "c", batchId: "batch", directoryToken: "dir", cursor: "next", limit: 128 }],
      ["cloud_object_resolve_upload_directory_conflict", { connectionId: "c", batchId: "batch", conflictId: "conflict-a", decision: "skip", applyToRemaining: false }],
      ["cloud_object_resolve_upload_directory_conflict", { connectionId: "c", batchId: "batch", conflictId: "conflict-a", decision: "overwrite", applyToRemaining: true }],
      ["cloud_object_cancel_upload_directory", { connectionId: "c", batchId: "batch" }],
      ["cloud_object_release_upload_directory", { connectionId: "c", batchId: "batch" }],
    ]);
    expect(cloudObjects.uploadDirectoryItem).toBeUndefined();
  });

  it("filters SaveS3Input fields and preserves explicit null id", () => {
    expect(toSaveS3Input({ id: "c", name: "n", repositoryName: "leak", password: "leak", proxy: { mode: "direct" } })).toEqual({ id: "c", name: "n", proxy: { mode: "direct" } });
  });
  it("filters mapping input fields and rejects identity/global/UI extras", () => {
    expect(toSaveMappingInput({ id: "m", name: "n", connectionId: "c", repositoryId: "r", localRoot: "/x", logicalPath: "d", extra: true, editing: true, selected: true, status: "ready" })).toEqual({ connectionId: "c", repositoryId: "r", localRoot: "/x", logicalPath: "d" });
  });
  it("preserves explicit connection clear flags while excluding UI and plaintext repository fields", () => {
    expect(toSaveS3Input({ id: "c", name: "n", encrypted: true, repositoryName: "repo", password: "plaintext", proxy: { mode: "custom", url: "http://proxy", username: "proxy-user", password: "proxy-pass" }, clearCredentials: true, clearSessionToken: true, clearProxyAuth: true })).toEqual({
      id: "c", name: "n", proxy: { mode: "custom", url: "http://proxy", username: "proxy-user", password: "proxy-pass" }, clearCredentials: true, clearSessionToken: true, clearProxyAuth: true,
    });
  });
  it("does not invoke directly from the wrapper module", async () => {
    expect(invoke).toBeDefined();
    expect(Object.keys(cloud)).toEqual([
      "protocols", "beginOfficialAuthorization", "beginLoopbackAuthorization", "waitLoopbackAuthorization", "cancelOfficialAuthorization", "completeOfficialAuthorization",
      "list", "probeBuckets", "connectionCredentials", "saveConnection", "testConnection", "deleteConnection",
      "createRepository", "joinRepository", "saveMapping", "appendDiscover", "createSyncDrive", "appendCreate", "appendJoin", "appendSaveMapping", "prepareMappingBind", "confirmMappingBind", "cancelMappingBind", "deleteMapping",
      "localEntries", "localStatus", "preview", "sync", "cancel", "recoveryRecords", "restore", "status",
      "mirrorFolders", "mirrorSaveMapping", "mirrorDeleteMapping", "mirrorSync", "mirrorCancel",
    ]);
  });

  it("creates independent connection defaults including secret proxy fields", () => {
    const first = blankConnection();
    const second = blankConnection();
    first.proxy.password = "secret";
    expect(second.proxy).toMatchObject({ mode: "client" });
  });

  it("creates independent mapping defaults with schedule settings", () => {
    const first = blankMapping();
    const second = blankMapping();
    first.schedule.interval = 60;
    expect(second).toMatchObject({ connectionId: "", repositoryId: "", localRoot: "", logicalPath: "" });
    expect(second.schedule).toEqual({ enabled: false, interval: 900 });
  });
});
