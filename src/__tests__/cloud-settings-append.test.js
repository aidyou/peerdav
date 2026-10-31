import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { nativeInvoke, app } = vi.hoisted(() => ({ nativeInvoke: vi.fn(), app: { handleFailure: vi.fn(), setNotice: vi.fn(), setError: vi.fn() } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: nativeInvoke }));
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("element-plus/es", async () => {
  const { defineComponent, h } = await import("vue");
  const control = (tag) => defineComponent({ props: ["modelValue", "type", "value", "label"], emits: ["update:modelValue"], setup(p, { emit, slots, attrs }) { return () => h(tag, { ...attrs, type: p.type, value: tag === "option" ? p.value : p.modelValue, checked: p.modelValue === true, onInput: (e) => emit("update:modelValue", e.target.value), onChange: (e) => emit("update:modelValue", e.target.type === "checkbox" ? e.target.checked : e.target.value) }, tag === "input" ? undefined : tag === "option" ? p.label : tag === "select" ? [h("option", { value: "" }, "Select"), slots.default?.()] : slots.default?.()); } });
  return { ElInput: control("input"), ElSelect: control("select"), ElOption: control("option"), ElButton: control("button"), ElSwitch: defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(p, { emit, attrs }) { return () => h("input", { ...attrs, type: "checkbox", checked: p.modelValue, onChange: (e) => emit("update:modelValue", e.target.checked) }); } }) };
});
import CloudSettings from "../components/CloudSettings.vue";
import { cloud } from "../lib/cloud";
import { errorStrategy, toUserMessage } from "../lib/invoke";
import { setLocale } from "../lib/i18n";
import en from "../locales/en";
import zh from "../locales/zh-CN";

const input = defineComponent({ props: ["modelValue", "type"], emits: ["update:modelValue"], setup(p, { emit, attrs }) { return () => h("input", { ...attrs, type: p.type || "text", value: p.modelValue, onInput: (e) => emit("update:modelValue", e.target.value) }); } });
const select = defineComponent({ name: "SelectStub", props: ["modelValue"], emits: ["update:modelValue"], setup(p, { emit, slots, attrs }) { return () => h("select", { ...attrs, value: p.modelValue, onChange: (e) => emit("update:modelValue", e.target.value) }, [h("option", { value: "" }, "Select"), slots.default?.()]); } });
const option = defineComponent({ props: ["value", "label"], setup(p) { return () => h("option", { value: p.value }, p.label); } });
const toggle = defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(p, { emit, attrs }) { return () => h("input", { ...attrs, type: "checkbox", checked: p.modelValue, onChange: (e) => emit("update:modelValue", e.target.checked) }); } });
const button = defineComponent({ setup(_, { slots, attrs }) { return () => h("button", attrs, slots.default?.()); } });
const anchor = (key, mode = "encrypted") => ({ key, fingerprint: `fp-${key}`, repo_id: `repo-${key}`, descriptor: { version: 2, repo_id: `repo-${key}`, mode, chunker: {} }, trust: "untrusted" });
let rows;
beforeEach(() => {
  vi.clearAllMocks(); setLocale("en");
  rows = { connections: [{ id: "c1", name: "Primary" }], repositories: [{ id: "cas-repo", name: "CAS repo" }], appendRepositories: [{ connectionId: "c1", protocol: "append_v2", binding: { repoId: "append-repo", rootCandidates: ["docs", "photos"], selectedRoot: "photos", appendMode: "encrypted" } }], mappings: [], statuses: [] };
  nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "cloud_append_discover" ? [anchor("one"), anchor("two", "authenticated_plain")] : {});
});
async function mountSettings() { const w = mount(CloudSettings, { global: { stubs: { ElInput: input, ElSelect: select, ElOption: option, ElSwitch: toggle, ElButton: button } } }); await flushPromises(); return w; }
async function appendSettings() { const w = await mountSettings(); await w.find(".repository-protocol").setValue("append_v2"); return w; }
async function fillPasswords(w) { await w.find(".append-password").setValue("password-123"); await w.find(".append-confirmation").setValue("password-123"); }
const saveMappingButton = (w) => w.findAll("button").filter((b) => b.text() === "Save").at(-1);
const mappingGroup = (w) => w.findAll(".settings-group").at(-1);

describe("append settings through unified invoke", () => {
  it("defaults to CAS and preserves its create payload", async () => {
    const w = await mountSettings(); expect(w.find(".repository-protocol").element.value).toBe("cas"); expect(w.find(".append-settings").exists()).toBe(false);
    const passwords = w.findAll('input[type="password"]'); await passwords[2].setValue("cas-pass"); await passwords[3].setValue("cas-pass");
    await w.findAll("button").find((b) => b.text() === "Create repository").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_create_repository", { connectionId: "c1", name: "", encrypted: true, password: "cas-pass", passwordConfirmation: "cas-pass" });
    expect(nativeInvoke.mock.calls.some(([c]) => c.startsWith("cloud_append"))).toBe(false); w.unmount();
  });
  it.each(["encrypted", "authenticated_plain"])("creates %s with both passwords and no CAS probe", async (mode) => {
    const w = await appendSettings(); await w.find(".append-mode").setValue(mode); expect(w.find(".append-create").attributes("disabled")).toBeDefined();
    if (mode === "authenticated_plain") expect(w.text()).toContain("still requires a password");
    await fillPasswords(w); await w.find(".append-create").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_create", { input: { connectionId: "c1", mode, password: "password-123", passwordConfirmation: "password-123" } });
    expect(w.find(".append-password").element.value).toBe(""); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("requires explicit candidate selection and TOFU, resets trust when candidate changes", async () => {
    const w = await appendSettings(); await fillPasswords(w); await w.find(".append-discover").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_discover", { connectionId: "c1" }); expect(w.find(".append-candidate").element.value).toBe(""); expect(w.find(".append-join").exists()).toBe(false);
    await w.find(".append-candidate").setValue("one"); expect(w.find(".append-tofu").element.checked).toBe(false); expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await w.find(".append-tofu").setValue(true); await w.find(".append-candidate").setValue("two"); expect(w.find(".append-tofu").element.checked).toBe(false);
    await w.find(".append-tofu").setValue(true); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: { connectionId: "c1", anchorKey: "two", anchorFingerprint: "fp-two", repositoryId: "repo-two", mode: "authenticated_plain", acceptTofu: true, trustedFingerprint: null, password: "password-123" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("joins using an independently entered fingerprint without implicit TOFU", async () => {
    const w = await appendSettings(); await fillPasswords(w); await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one");
    await w.find(".append-fingerprint").setValue("trusted-fp"); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: expect.objectContaining({ acceptTofu: false, trustedFingerprint: "trusted-fp" }) }); w.unmount();
  });
  it("does not preselect any authenticated root and routes append mapping separately", async () => {
    const w = await mountSettings(); await w.find(".mapping-protocol").setValue("append_v2"); let selects = mappingGroup(w).findAll("select"); await selects[1].setValue("c1"); await selects[2].setValue("append-repo");
    expect(w.find(".append-root").element.value).toBe(""); await saveMappingButton(w).trigger("click"); expect(app.setError).toHaveBeenCalledWith("cloudAppend.rootRequired"); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    await w.find(".append-root").setValue("photos"); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "append-repo", localRoot: "", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_save_mapping" || c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("allows an explicitly named root only for an empty append repository", async () => {
    rows.appendRepositories[0].binding.rootCandidates = []; const w = await mountSettings(); await w.find(".mapping-protocol").setValue("append_v2"); const group = mappingGroup(w); await group.findAll("select")[1].setValue("c1"); await group.findAll("select")[2].setValue("append-repo");
    await group.findAll("label").find((l) => l.text().includes("Remote path")).find("input").setValue("new-root"); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: expect.objectContaining({ selectedRoot: "new-root", mapping: expect.objectContaining({ logicalPath: "new-root" }) }) }); w.unmount();
  });
  it("preserves default CAS mapping payload", async () => {
    const w = await mountSettings(); await saveMappingButton(w).trigger("click"); await flushPromises(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_save_mapping", { mappingId: null, input: { connectionId: "", repositoryId: "", localRoot: "", logicalPath: "", schedule: { enabled: false, interval: 900 } } }); w.unmount();
  });
  it("shows loading/disabled, empty and discovery failure states without stale candidates", async () => {
    const w = await appendSettings(); let resolve; nativeInvoke.mockImplementation((c) => c === "cloud_append_discover" ? new Promise((r) => { resolve = r; }) : Promise.resolve(rows));
    await w.find(".append-discover").trigger("click"); expect(w.text()).toContain("in progress"); expect(w.find(".append-discover").attributes("disabled")).toBeDefined(); resolve([]); await flushPromises(); expect(w.text()).toContain("No append repository anchors found");
    const failure = { code: "CLOUD_APPEND_DEPENDENCY", message: "Missing dependency", detail: null, context: null }; const log = vi.spyOn(console, "error").mockImplementation(() => {}); nativeInvoke.mockRejectedValueOnce(failure);
    await w.find(".append-discover").trigger("click"); await flushPromises(); expect(w.text()).toContain("Append operation failed"); expect(app.handleFailure).toHaveBeenCalledWith(failure); expect(w.find(".append-candidate").exists()).toBe(false); expect(w.find(".append-discover").attributes("disabled")).toBeUndefined(); log.mockRestore(); w.unmount();
  });
  it.each(["cloud_append_create", "cloud_append_join"])("shows %s authentication failures and retains retry input", async (command) => {
    const w = await appendSettings(); await fillPasswords(w);
    if (command === "cloud_append_join") { await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one"); await w.find(".append-tofu").setValue(true); }
    const failure = { code: "CLOUD_APPEND_AUTHENTICATION", message: "Wrong password", detail: null, context: null }; const log = vi.spyOn(console, "error").mockImplementation(() => {}); nativeInvoke.mockRejectedValueOnce(failure);
    await w.find(command === "cloud_append_create" ? ".append-create" : ".append-join").trigger("click"); await flushPromises(); expect(app.handleFailure).toHaveBeenCalledWith(failure); expect(w.text()).toContain("Append operation failed"); expect(w.find(".append-password").element.value).toBe("password-123"); log.mockRestore(); w.unmount();
  });
  it("passes opaque recovery IDs unchanged via the shared wrapper", async () => { await cloud.restore("append-mapping", "opaque:/not-a-path?record", "restored.txt"); expect(nativeInvoke).toHaveBeenCalledWith("cloud_restore", { mappingId: "append-mapping", recordId: "opaque:/not-a-path?record", destination: "restored.txt" }); });
  it("has matching bilingual keys and stable append error strategies", () => {
    expect(Object.keys(en).filter((k) => k.startsWith("cloudAppend.") || k.startsWith("errors.CLOUD_APPEND")).sort()).toEqual(Object.keys(zh).filter((k) => k.startsWith("cloudAppend.") || k.startsWith("errors.CLOUD_APPEND")).sort());
    for (const [code, strategy] of Object.entries({ CLOUD_APPEND_PASSWORD: "edit", CLOUD_APPEND_ROOT_SELECTION: "edit", CLOUD_APPEND_TRUST_REQUIRED: "edit", CLOUD_APPEND_FINGERPRINT: "edit", CLOUD_APPEND_AUTHENTICATION: "retry", CLOUD_APPEND_DEPENDENCY: "support", CLOUD_APPEND: "show" })) {
      expect(errorStrategy({ code, message: "reason" })).toBe(strategy); expect(toUserMessage({ code, message: "original reason" })).toContain("original reason");
    }
  });
});
