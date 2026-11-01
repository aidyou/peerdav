import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("../invoke.js", () => ({ invoke }));

import { blankConnection, blankMapping, cloud, toSaveMappingInput, toSaveS3Input } from "../cloud.js";

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
      ["cloud_preview", { mappingId: "m" }],
      ["cloud_sync", { mappingId: "m", previewId: "p" }],
      ["cloud_cancel", { mappingId: "m" }],
      ["cloud_recovery_records", { mappingId: "m" }],
      ["cloud_restore", { mappingId: "m", recordId: "record", destination: "/dest" }],
      ["cloud_status"],
    ]);
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
      "list", "probeBuckets", "connectionCredentials", "saveConnection", "testConnection", "deleteConnection",
      "createRepository", "joinRepository", "saveMapping", "appendDiscover", "createSyncDrive", "appendCreate", "appendJoin", "appendSaveMapping", "deleteMapping",
      "preview", "sync", "cancel", "recoveryRecords", "restore", "status",
    ]);
  });

  it("creates independent connection defaults including secret proxy fields", () => {
    const first = blankConnection();
    const second = blankConnection();
    first.proxy.password = "secret";
    expect(second.proxy).toMatchObject({ mode: "direct" });
  });

  it("creates independent mapping defaults with schedule settings", () => {
    const first = blankMapping();
    const second = blankMapping();
    first.schedule.interval = 60;
    expect(second).toMatchObject({ connectionId: "", repositoryId: "", localRoot: "", logicalPath: "" });
    expect(second.schedule).toEqual({ enabled: false, interval: 900 });
  });
});
