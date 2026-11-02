import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { nativeInvoke, app, openDialog } = vi.hoisted(() => ({ nativeInvoke: vi.fn(), openDialog: vi.fn(), app: { handleFailure: vi.fn(), setNotice: vi.fn(), setError: vi.fn() } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: nativeInvoke }));
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openDialog }));
vi.mock("element-plus/es", async () => {
  const { defineComponent, h } = await import("vue");
  const control = (tag) => defineComponent({ props: ["modelValue", "type", "value", "label"], emits: ["update:modelValue"], setup(p, { emit, slots, attrs }) { return () => h(tag, { ...attrs, type: p.type, value: tag === "option" ? p.value : p.modelValue, checked: p.modelValue === true, onInput: (e) => emit("update:modelValue", e.target.value), onChange: (e) => emit("update:modelValue", e.target.type === "checkbox" ? e.target.checked : e.target.value) }, tag === "input" ? undefined : tag === "option" ? p.label : tag === "select" ? [h("option", { value: "" }, "Select"), slots.default?.()] : slots.default?.()); } });
  return { ElDialog: defineComponent({ props: ['modelValue'], setup(p, { slots, attrs }) { return () => p.modelValue ? h('section', attrs, [slots.default?.(), slots.footer?.()]) : null; } }), ElInput: control("input"), ElSelect: control("select"), ElOption: control("option"), ElButton: control("button"), ElSwitch: defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(p, { emit, attrs }) { return () => h("input", { ...attrs, type: "checkbox", checked: p.modelValue, onChange: (e) => emit("update:modelValue", e.target.checked) }); } }) };
});
import CloudSettings from "../components/CloudSyncDrives.vue";
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
  rows = { connections: [{ id: "c1", name: "Primary" }], repositories: [{ id: "cas-repo", connectionId: "c1", name: "Legacy drive" }], appendRepositories: [{ connectionId: "c1", protocol: "append_v2", binding: { repoId: "append-repo", rootCandidates: ["docs", "photos"], selectedRoot: "photos", appendMode: "encrypted" } }], mappings: [], statuses: [] };
  nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "cloud_append_discover" ? [anchor("one"), anchor("two", "authenticated_plain")] : command === "cloud_create_sync_drive" ? { connectionId: "c1", protocol: "append_v2", binding: { repoId: "new-repo" } } : {});
});
const changed = vi.fn();
const dialog = defineComponent({ props: ['modelValue'], setup(p, { slots, attrs }) { return () => p.modelValue ? h('section', attrs, [slots.default?.(), slots.footer?.()]) : null; } });
async function mountSettings() { const w = mount(CloudSettings, { props: { connectionId: "c1", onChanged: changed }, global: { stubs: { ElDialog: dialog, ElInput: input, ElSelect: select, ElOption: option, ElSwitch: toggle, ElButton: button } } }); await flushPromises(); await w.find('.open-create-drive').trigger('click'); return w; }
async function appendSettings() { const w = await mountSettings(); await w.find('.drive-connect').trigger('click'); return w; }
async function fillPasswords(w) { await w.find(".append-password").setValue("password-123"); await w.find(".append-confirmation").setValue("password-123"); const join = w.find(".append-join-password"); if (join.exists()) await join.setValue("password-123"); }
const saveMappingButton = (w) => w.findAll("button").filter((b) => b.text() === "Save").at(-1);

describe("append settings through unified invoke", () => {
  it('cancels the creation dialog without submitting and clears sensitive inputs', async () => {
    const w = await mountSettings(); await w.find('.new-drive-local').setValue('/local'); await fillPasswords(w);
    await w.find('.cancel-create-drive').trigger('click');
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false);
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_create_sync_drive')).toBe(false);
    await w.find('.open-create-drive').trigger('click'); expect(w.find('.append-password').element.value).toBe(''); w.unmount();
  });
  it('submits once while creation is pending and closes after successful binding', async () => {
    const w = await mountSettings(); await w.find('.new-drive-local').setValue('/local'); await fillPasswords(w);
    let finish; nativeInvoke.mockImplementation((c) => c === 'cloud_create_sync_drive' ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve(c === 'cloud_list' ? rows : {}));
    await w.find('.append-create').trigger('click'); await w.find('form').trigger('submit');
    expect(nativeInvoke.mock.calls.filter(([c]) => c === 'cloud_create_sync_drive')).toHaveLength(1);
    expect(w.find('.cancel-create-drive').attributes('disabled')).toBeDefined();
    finish({ binding: { repoId: 'created' } }); await flushPromises();
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false); w.unmount();
  });
  it("hides protocol controls and scopes existing drives to the open connection", async () => {
    rows.repositories.push({ id: "other", connectionId: "other", name: "Other drive" });
    const w = await mountSettings(); expect(w.find(".repository-protocol").exists()).toBe(false); expect(w.find(".mapping-protocol").exists()).toBe(false);
    expect(w.text()).not.toMatch(/CAS|Append V2|Other drive|append-repo/); expect(w.text()).toContain("Sync drive 1"); expect(w.text()).not.toContain("Legacy drive"); w.unmount();
  });
  it('highlights only the focused drive without hiding other drives or selecting a mapping', async () => {
    const w = await mountSettings(); await w.setProps({ focusId: 'append-repo' });
    expect(w.findAll('.drive-list li')).toHaveLength(2);
    expect(w.find('.drive-list .is-active').text()).toContain('Sync drive 2');
    await w.find('.new-mapping').trigger('click');
    expect(w.find('.mapping-repository').element.value).toBe('');
    await w.setProps({ focusId: 'cas-repo' }); expect(w.find('.drive-list .is-active').text()).toContain('Sync drive 1');
    await w.setProps({ focusId: '' }); expect(w.find('.drive-list .is-active').exists()).toBe(false); w.unmount();
  });
  it.each(["encrypted", "authenticated_plain"])("creates %s and binds the first local folder in one step", async (mode) => {
    const w = await appendSettings(); await w.find(".new-drive-local").setValue("/local/Documents"); await w.find(".append-mode").setValue(mode);
    expect(w.find(".append-create").attributes("disabled")).toBeDefined();
    if (mode === "authenticated_plain") expect(w.text()).toContain("still requires a password");
    await fillPasswords(w); expect(w.find(".append-create").attributes("disabled")).toBeUndefined();
    await w.find(".append-create").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_create_sync_drive", { input: { connectionId: "c1", mode, password: "password-123", passwordConfirmation: "password-123" } });
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "new-repo", localRoot: "/local/Documents", logicalPath: "Documents", schedule: { enabled: false, interval: 900 } }, selectedRoot: "Documents" } });
    await flushPromises();
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('.append-state').text()).toBe('Sync drive saved.');
    expect(changed).toHaveBeenCalledTimes(1);
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("requires explicit candidate selection and TOFU, resets trust when candidate changes", async () => {
    const w = await appendSettings(); await fillPasswords(w); await w.find(".append-discover").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_discover", { connectionId: "c1" }); expect(w.find(".append-candidate").element.value).toBe(""); expect(w.find(".append-join").exists()).toBe(false);
    await w.find(".append-candidate").setValue("one"); expect(w.find('.advanced-security').attributes('open')).toBeUndefined(); expect(w.find('.append-fingerprint').element.closest('details')).toBe(w.find('.advanced-security').element); expect(w.find(".append-tofu").element.checked).toBe(false); expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await w.find(".append-tofu").setValue(true); await w.find(".append-candidate").setValue("two"); expect(w.find(".append-tofu").element.checked).toBe(false);
    await w.find(".append-tofu").setValue(true); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: { connectionId: "c1", anchorKey: "two", anchorFingerprint: "fp-two", repositoryId: "repo-two", mode: "authenticated_plain", acceptTofu: true, trustedFingerprint: null, password: "password-123" } });
    await flushPromises();
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('.append-state').text()).toBe('Sync drive saved.');
    expect(changed).toHaveBeenCalledTimes(1);
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("joins using an independently entered fingerprint without implicit TOFU", async () => {
    const w = await appendSettings(); await fillPasswords(w); await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one");
    await w.find(".append-fingerprint").setValue("trusted-fp"); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: expect.objectContaining({ acceptTofu: false, trustedFingerprint: "trusted-fp" }) }); w.unmount();
  });
  it("does not preselect any authenticated root and routes append mapping separately", async () => {
    const w = await mountSettings(); await w.find('.new-mapping').trigger('click'); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local'); expect(w.find(".append-root").element.value).toBe(""); await saveMappingButton(w).trigger("click"); expect(app.setError).toHaveBeenCalledWith("cloudAppend.rootRequired"); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    await w.find(".append-root").setValue("photos"); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "append-repo", localRoot: "/local", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_save_mapping" || c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("allows an explicitly named root only for an empty append repository", async () => {
    rows.appendRepositories[0].binding.rootCandidates = []; const w = await mountSettings(); await w.find('.new-mapping').trigger('click'); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local');
    await w.find('.mapping-remote').setValue('new-root'); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: expect.objectContaining({ selectedRoot: "new-root", mapping: expect.objectContaining({ logicalPath: "new-root" }) }) }); w.unmount();
  });
  it("edits an existing CAS mapping without changing its protocol and previews the selected mapping", async () => {
    rows.mappings = [{ id: 'legacy', connectionId: 'c1', repositoryId: 'cas-repo', localRoot: '/old', logicalPath: 'docs', schedule: {} }];
    const w = await mountSettings(); await w.find('.mapping-item').findAll('button')[0].trigger('click');
    await w.find('.mapping-local').setValue('/new'); await w.find('.mapping-save').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_save_mapping', { mappingId: 'legacy', input: { connectionId: 'c1', repositoryId: 'cas-repo', localRoot: '/new', logicalPath: 'docs', schedule: { enabled: false, interval: 900 } } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_append_save_mapping')).toBe(false);
    nativeInvoke.mockResolvedValueOnce({ previewId: 'p1', summary: { uploads: 1 } });
    await w.find('.mapping-item').findAll('button')[2].trigger('click'); await flushPromises();
    await w.find('.cloud-preview button').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_preview', { mappingId: 'legacy' });
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_sync', { mappingId: 'legacy', previewId: 'p1' }); w.unmount();
  });
  it("shows loading/disabled, empty and discovery failure states without stale candidates", async () => {
    const w = await appendSettings(); let resolve; nativeInvoke.mockImplementation((c) => c === "cloud_append_discover" ? new Promise((r) => { resolve = r; }) : Promise.resolve(rows));
    await w.find(".append-discover").trigger("click"); expect(w.text()).toContain("in progress"); expect(w.find(".append-discover").attributes("disabled")).toBeDefined(); resolve([]); await flushPromises(); expect(w.text()).toContain("No existing sync drives found");
    const failure = { code: "CLOUD_APPEND_DEPENDENCY", message: "Missing dependency", detail: null, context: null }; const log = vi.spyOn(console, "error").mockImplementation(() => {}); nativeInvoke.mockRejectedValueOnce(failure);
    await w.find(".append-discover").trigger("click"); await flushPromises(); expect(w.text()).toContain("Sync drive operation failed"); expect(app.handleFailure).toHaveBeenCalledWith(failure); expect(w.find(".append-candidate").exists()).toBe(false); expect(w.find(".append-discover").attributes("disabled")).toBeUndefined(); log.mockRestore(); w.unmount();
  });
  it.each(["cloud_create_sync_drive", "cloud_append_join"])("shows %s authentication failures and retains retry input", async (command) => {
    const w = await appendSettings(); await w.find(".new-drive-local").setValue("/local/Documents"); await fillPasswords(w);
    if (command === "cloud_append_join") { await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one"); await w.find(".append-tofu").setValue(true); }
    const failure = { code: "CLOUD_APPEND_AUTHENTICATION", message: "Wrong password", detail: null, context: null }; const log = vi.spyOn(console, "error").mockImplementation(() => {}); nativeInvoke.mockRejectedValueOnce(failure);
    await w.find(command === "cloud_create_sync_drive" ? ".append-create" : ".append-join").trigger("click"); await flushPromises(); expect(app.handleFailure).toHaveBeenCalledWith(failure); expect(w.text()).toContain("Sync drive operation failed"); expect(w.find(".append-password").element.value).toBe("password-123"); log.mockRestore(); w.unmount();
  });
  it('selects a local directory and edits an existing append mapping using its configured protocol', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/old', logicalPath: 'photos', schedule: { enabled: true, interval: 120 } }];
    openDialog.mockResolvedValue('/chosen'); const w = await mountSettings();
    await w.find('.mapping-item button').trigger('click');
    expect(w.find('.append-root').element.value).toBe('photos'); expect(w.find('.mapping-repository').attributes('disabled')).toBeDefined();
    await w.findAll('button').find((b) => b.classes().includes('choose-mapping-root')).trigger('click'); await flushPromises();
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false });
    await w.find('.mapping-save').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_append_save_mapping', { input: { mappingId: 'a1', mapping: { connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/chosen', logicalPath: 'photos', schedule: { enabled: true, interval: 120 } }, selectedRoot: 'photos' } }); w.unmount();
  });
  it('rejects mismatched create passwords and retains inputs on backend failure', async () => {
    const w = await mountSettings(); await w.find('.append-password').setValue('one'); await w.find('.append-confirmation').setValue('two');
    expect(w.find('.append-create').attributes('disabled')).toBeDefined();
    await w.find('.append-create').trigger('click'); expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_create_sync_drive')).toBe(false); w.unmount();
  });
  it("passes opaque recovery IDs unchanged via the shared wrapper", async () => { await cloud.restore("append-mapping", "opaque:/not-a-path?record", "restored.txt"); expect(nativeInvoke).toHaveBeenCalledWith("cloud_restore", { mappingId: "append-mapping", recordId: "opaque:/not-a-path?record", destination: "restored.txt" }); });
  it("has matching bilingual keys and stable append error strategies", () => {
    expect(Object.keys(en).filter((k) => k.startsWith("cloudAppend.") || k.startsWith("errors.CLOUD_APPEND")).sort()).toEqual(Object.keys(zh).filter((k) => k.startsWith("cloudAppend.") || k.startsWith("errors.CLOUD_APPEND")).sort());
    for (const [code, strategy] of Object.entries({ CLOUD_APPEND_PASSWORD: "edit", CLOUD_APPEND_ROOT_SELECTION: "edit", CLOUD_APPEND_TRUST_REQUIRED: "edit", CLOUD_APPEND_FINGERPRINT: "edit", CLOUD_APPEND_AUTHENTICATION: "retry", CLOUD_APPEND_DEPENDENCY: "support", CLOUD_APPEND: "show" })) {
      expect(errorStrategy({ code, message: "reason" })).toBe(strategy); expect(toUserMessage({ code, message: "original reason" })).toContain("original reason");
    }
  });
});
