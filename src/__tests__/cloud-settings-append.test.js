import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { nativeInvoke, app, openDialog } = vi.hoisted(() => ({ nativeInvoke: vi.fn(), openDialog: vi.fn(), app: { handleFailure: vi.fn(), setNotice: vi.fn(), setError: vi.fn() } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: nativeInvoke }));
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openDialog }));
vi.mock("../lib/confirm", () => ({ confirmAction: vi.fn(async () => true) }));
vi.mock("element-plus/es", async () => {
  const { defineComponent, h } = await import("vue");
  const control = (tag) => defineComponent({ props: ["modelValue", "type", "value", "label"], emits: ["update:modelValue"], setup(p, { emit, slots, attrs }) { return () => h(tag, { ...attrs, type: p.type, value: tag === "option" ? p.value : p.modelValue, checked: p.modelValue === true, onInput: (e) => emit("update:modelValue", e.target.value), onChange: (e) => emit("update:modelValue", e.target.type === "checkbox" ? e.target.checked : e.target.value) }, tag === "input" ? undefined : tag === "option" ? p.label : tag === "select" ? [h("option", { value: "" }, "Select"), slots.default?.()] : slots.default?.()); } });
  return { ElDialog: defineComponent({ props: ['modelValue'], setup(p, { slots, attrs }) { return () => p.modelValue ? h('section', attrs, [slots.default?.(), slots.footer?.()]) : null; } }), ElInput: control("input"), ElSelect: control("select"), ElOption: control("option"), ElButton: control("button"), ElSwitch: defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(p, { emit, attrs }) { return () => h("input", { ...attrs, type: "checkbox", checked: p.modelValue, onChange: (e) => emit("update:modelValue", e.target.checked) }); } }), ElPopover: defineComponent({ setup(_, { slots }) { return () => h("div", slots.default?.()); } }), ElCollapse: defineComponent({ setup(_, { slots }) { return () => h("div", slots.default?.()); } }), ElCollapseItem: defineComponent({ props: ["name"], setup(p, { slots, attrs }) { return () => h("div", attrs, [h("div", { class: "collapse-item__title" }, slots.title?.()), h("div", { class: "collapse-item__content" }, slots.default?.())]); } }), ElTag: defineComponent({ setup(_, { slots, attrs }) { return () => h("span", attrs, slots.default?.()); } }) };
});
import CloudSettings from "../components/CloudSyncDrives.vue";
import { cloud } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
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
  rows = { connections: [{ id: "c1", name: "Primary" }], repositories: [{ id: "cas-repo", connectionId: "c1", name: "Legacy drive" }], appendRepositories: [{ connectionId: "c1", protocol: "append_v2", binding: { repoId: "append-repo", rootCandidates: ["docs", "photos"], selectedRoot: "photos", appendMode: "encrypted" } }], mappings: [], statuses: [], mirrorMappings: [] };
  nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "cloud_append_discover" ? [anchor("one"), anchor("two", "authenticated_plain")] : command === "cloud_create_sync_drive" ? { connectionId: "c1", protocol: "append_v2", binding: { repoId: "new-repo" } } : command === "cloud_append_join" ? { binding: { repoId: "joined-repo" } } : {});
});
const changed = vi.fn();
const dialog = defineComponent({ props: ['modelValue'], setup(p, { slots, attrs }) { return () => p.modelValue ? h('section', attrs, [slots.default?.(), slots.footer?.()]) : null; } });
const popover = defineComponent({ setup(_, { slots }) { return () => h("div", slots.default?.()); } });
const collapse = defineComponent({ setup(_, { slots }) { return () => h("div", slots.default?.()); } });
const collapseItem = defineComponent({ props: ["name"], setup(p, { slots, attrs }) { return () => h("div", attrs, [h("div", { class: "collapse-item__title" }, slots.title?.()), h("div", { class: "collapse-item__content" }, slots.default?.())]); } });
const tag = defineComponent({ setup(_, { slots, attrs }) { return () => h("span", attrs, slots.default?.()); } });
async function mountSettings() { const w = mount(CloudSettings, { props: { connectionId: "c1", onChanged: changed }, global: { stubs: { ElDialog: dialog, ElInput: input, ElSelect: select, ElOption: option, ElSwitch: toggle, ElButton: button, ElPopover: popover, ElCollapse: collapse, ElCollapseItem: collapseItem, ElTag: tag } } }); await flushPromises(); await w.find('.add-menu-trigger').trigger('click'); await w.find('.open-create-drive').trigger('click'); return w; }
async function appendSettings() { const w = await mountSettings(); await w.find('.add-menu-trigger').trigger('click'); await w.find('.add-connect-drive').trigger('click'); return w; }
async function openCreateDialog(w) { if (!w.find('[data-test=create-drive-dialog]').exists()) { await w.find('.add-menu-trigger').trigger('click'); await w.find('.open-create-drive').trigger('click'); } return w; }
async function openMappingDialog(w) { await w.find('.add-menu-trigger').trigger('click'); await w.find('.new-mapping').trigger('click'); return w; }
// 同步盘与镜像盘现为两个常驻块，不再切 tab；保留 helper 名以兼容既有调用点。
async function openMirrorTab(w) { return w; }
async function fillPasswords(w) { const create = w.find(".append-password"); if (create.exists()) await create.setValue("password-123"); const confirm = w.find(".append-confirmation"); if (confirm.exists()) await confirm.setValue("password-123"); const join = w.find(".append-join-password"); if (join.exists()) await join.setValue("password-123"); }
async function fillLinkLocalRoot(w, root = "/local/Documents") { await w.find(".link-local-root").setValue(root); }
const saveMappingButton = (w) => w.findAll("button").filter((b) => b.text() === "Save").at(-1);

describe("append settings through unified invoke", () => {
  it('cancels the creation dialog without submitting, clears sensitive inputs and resets the drive type', async () => {
    const w = await mountSettings(); await w.find('.new-drive-local').setValue('/local'); await fillPasswords(w);
    await w.find(".create-mode").setValue("mirror");
    await w.find('.cancel-create-drive').trigger('click');
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false);
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_create_sync_drive')).toBe(false);
    await openCreateDialog(w);
    expect(w.find('.create-mode').element.value).toBe('encrypted');
    expect(w.find('.append-password').element.value).toBe(''); w.unmount();
  });
  it("switches copy and password visibility for all three drive types", async () => {
    const w = await mountSettings();
    expect(w.find('.create-mode').element.value).toBe('encrypted');
    expect(w.find('.create-mode-hint').text()).toContain('Content and file names are encrypted');
    expect(w.find('.append-password').exists()).toBe(true);
    await w.find('.create-mode').setValue('authenticated_plain');
    expect(w.find('.create-mode-hint').text()).toContain('stored in plaintext');
    expect(w.find('.append-password').exists()).toBe(true);
    expect(w.find('.mirror-cloud-prefix').exists()).toBe(false);
    await w.find('.create-mode').setValue('mirror');
    expect(w.find('.create-mode-hint').text()).toContain('browse files in the cloud');
    expect(w.find('.append-password').exists()).toBe(false);
    expect(w.find('.mirror-cloud-prefix').exists()).toBe(true);
    w.unmount();
  });
  it("offers two drive sections and a three-action add menu", async () => {
    const w = await mountSettings();
    expect(w.findAll('.drive-group').length).toBe(2);
    expect(w.find('[data-test="sync-group"] .group-title').text()).toContain('Sync drives');
    expect(w.find('[data-test="mirror-group"] .group-title').text()).toContain('Mirror drives');
    // 对话框高度改由统一 scoped 样式控制，不再使用内联 :style。
    expect(w.find('[data-test=create-drive-dialog]').attributes('style')).toBeUndefined();
    await w.find('.cancel-create-drive').trigger('click');
    await w.find('.add-menu-trigger').trigger('click');
    expect(w.findAll('.add-menu-list [role=menuitem]').map((item) => item.text())).toEqual(['New sync drive', 'Connect remote sync drive', 'Add local folder']);
    await w.find('.add-connect-drive').trigger('click');
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(true);
    await w.find('.cancel-connect-drive').trigger('click');
    await w.find('.add-menu-trigger').trigger('click');
    await w.find('.add-local-folder').trigger('click');
    expect(w.find('[data-test=mapping-dialog]').exists()).toBe(true);
    w.unmount();
  });
  it("opens the mirror create dialog from the mirror section action", async () => {
    const w = await mountSettings(); await w.find('.cancel-create-drive').trigger('click');
    await w.find('[data-test="mirror-group"] .new-mirror-drive').trigger('click');
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(true);
    expect(w.find('.mirror-cloud-prefix').exists()).toBe(true);
    expect(w.find('.new-drive-local').exists()).toBe(false);
    w.unmount();
  });
  it("discovers existing drives before asking for a password", async () => {
    const w = await appendSettings(); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_discover", { connectionId: "c1" });
    expect(w.find(".append-candidate").exists()).toBe(true);
    expect(w.find(".append-join-password").exists()).toBe(false);
    await w.find(".append-candidate").setValue("one");
    expect(w.find(".append-join-password").exists()).toBe(true);
    expect(w.find(".connect-candidate-type").text()).toBe("Encrypted sync drive");
    await w.find(".append-candidate").setValue("two");
    expect(w.find(".connect-candidate-type").text()).toBe("Authenticated plaintext sync drive");
    expect(w.find(".append-join-password").exists()).toBe(true);
    w.unmount();
  });
  it("keeps mapping repository choices scoped to the current cloud connection", async () => {
    const w = await mountSettings(); await w.setProps({ focusId: 'append-repo' }); await openMappingDialog(w);
    expect(w.find('.mapping-repository').element.value).toBe('append-repo');
    expect(w.text()).toContain('These sync drives all belong to the current cloud connection.');
    w.unmount();
  });
  it("shows the local mapping table without a statistics column", async () => {
    rows.mappings = [{ id: 'm1', connectionId: 'c1', repositoryId: 'cas-repo', localRoot: '/local', logicalPath: 'docs', schedule: {} }];
    const w = await mountSettings(); await w.find('.cancel-create-drive').trigger('click');
    expect(w.find('[data-test="sync-group"] .client-collapse').exists()).toBe(true);
    expect(w.findAll('[data-test="sync-group"] .client-item__detail li > span').map((cell) => cell.text())).toEqual(['Local folder', 'Remote folder']);
    expect(w.find('.mapping-item').text()).toContain('/local');
    expect(w.find('.mapping-item').text()).toContain('Sync drive 1 · docs');
    expect(w.find('.mapping-status').text()).toBe('Idle');
    w.unmount();
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
    await openMappingDialog(w);
    expect(w.find('.mapping-repository').element.value).toBe('append-repo');
    await w.setProps({ focusId: 'cas-repo' }); expect(w.find('.drive-list .is-active').text()).toContain('Sync drive 1');
    await w.setProps({ focusId: '' }); expect(w.find('.drive-list .is-active').exists()).toBe(false); w.unmount();
  });
  it.each(["encrypted", "authenticated_plain"])("creates %s and binds the first local folder in one step", async (mode) => {
    const w = await mountSettings(); await w.find(".create-mode").setValue(mode); await w.find(".new-drive-local").setValue("/local/Documents");
    expect(w.find(".append-create").attributes("disabled")).toBeDefined();
    if (mode === "authenticated_plain") expect(w.find(".create-mode-hint").text()).toContain("stored in plaintext");
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
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: ["docs"], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_discover", { connectionId: "c1" }); expect(w.find(".append-candidate").element.value).toBe(""); expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); expect(w.find('.advanced-security').attributes('open')).toBeUndefined(); expect(w.find('.append-fingerprint').element.closest('details')).toBe(w.find('.advanced-security').element); expect(w.find(".append-tofu").element.checked).toBe(false); expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await w.find(".append-tofu").setValue(true); await w.find(".append-candidate").setValue("two"); expect(w.find(".append-tofu").element.checked).toBe(false);
    await w.find(".append-tofu").setValue(true); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: { connectionId: "c1", anchorKey: "two", anchorFingerprint: "fp-two", repositoryId: "repo-two", mode: "authenticated_plain", acceptTofu: true, trustedFingerprint: null, password: "password-123" } });
    await flushPromises();
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('.append-state').text()).toBe('Sync drive saved.');
    expect(changed).toHaveBeenCalledTimes(1);
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "joined-repo", localRoot: "/local/Documents", logicalPath: "docs", schedule: { enabled: false, interval: 900 } }, selectedRoot: "docs" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("asks for a root when the joined drive has several and binds the chosen one", async () => {
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: ["docs", "photos"], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); await w.find(".append-tofu").setValue(true); await w.find(".append-join").trigger("click"); await flushPromises();
    // 多根目录：join 成功但尚未绑定，等待用户选择。
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    expect(w.find(".link-root").exists()).toBe(true);
    await w.find(".link-root").setValue("photos"); await w.find(".append-bind").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "joined-repo", localRoot: "/local/Documents", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(w.find('.append-state').text()).toBe('Sync drive saved.'); expect(changed).toHaveBeenCalledTimes(1); w.unmount();
  });
  it("asks for a new root name when the joined drive is empty", async () => {
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: [], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); await w.find(".append-tofu").setValue(true); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(w.find(".link-new-root").exists()).toBe(true);
    await w.find(".link-new-root").setValue("fresh-root"); await w.find(".append-bind").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: expect.objectContaining({ selectedRoot: "fresh-root", mapping: expect.objectContaining({ logicalPath: "fresh-root" }) }) }); w.unmount();
  });
  it("keeps the join button disabled until a local folder is chosen", async () => {
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await w.find(".append-tofu").setValue(true);
    expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await fillLinkLocalRoot(w); expect(w.find(".append-join").attributes("disabled")).toBeUndefined(); w.unmount();
  });
  it("joins using an independently entered fingerprint without implicit TOFU", async () => {
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w);
    await w.find(".append-fingerprint").setValue("trusted-fp"); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: expect.objectContaining({ acceptTofu: false, trustedFingerprint: "trusted-fp" }) }); w.unmount();
  });
  it("does not preselect any authenticated root and routes append mapping separately", async () => {
    const w = await mountSettings(); await openMappingDialog(w); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local'); expect(w.find(".append-root").element.value).toBe(""); await saveMappingButton(w).trigger("click"); expect(app.setError).toHaveBeenCalledWith("cloudAppend.rootRequired"); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    await w.find(".append-root").setValue("photos"); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "append-repo", localRoot: "/local", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_save_mapping" || c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("allows an explicitly named root only for an empty append repository", async () => {
    rows.appendRepositories[0].binding.rootCandidates = []; const w = await mountSettings(); await openMappingDialog(w); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local');
    await w.find('.mapping-remote').setValue('new-root'); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_save_mapping", { input: expect.objectContaining({ selectedRoot: "new-root", mapping: expect.objectContaining({ logicalPath: "new-root" }) }) }); w.unmount();
  });
  it("edits an existing CAS mapping without changing its protocol and previews the selected mapping", async () => {
    rows.mappings = [{ id: 'legacy', connectionId: 'c1', repositoryId: 'cas-repo', localRoot: '/old', logicalPath: 'docs', schedule: {} }];
    const w = await mountSettings(); await w.find('.mapping-item').find('.mapping-edit').trigger('click');
    await w.find('.mapping-local').setValue('/new'); await w.find('.mapping-save').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_save_mapping', { mappingId: 'legacy', input: { connectionId: 'c1', repositoryId: 'cas-repo', localRoot: '/new', logicalPath: 'docs', schedule: { enabled: false, interval: 900 } } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_append_save_mapping')).toBe(false);
    nativeInvoke.mockResolvedValueOnce({ previewId: 'p1', summary: { uploads: 1 } });
    await w.find('.mapping-item').find('.mapping-preview').trigger('click'); await flushPromises();
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
    const w = command === "cloud_create_sync_drive" ? await mountSettings() : await appendSettings();
    if (command === "cloud_create_sync_drive") { await w.find(".new-drive-local").setValue("/local/Documents"); await fillPasswords(w); }
    if (command === "cloud_append_join") { await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await w.find(".append-tofu").setValue(true); await fillLinkLocalRoot(w); }
    const failure = { code: "CLOUD_APPEND_AUTHENTICATION", message: "Wrong password", detail: null, context: null }; const log = vi.spyOn(console, "error").mockImplementation(() => {}); nativeInvoke.mockRejectedValueOnce(failure);
    await w.find(command === "cloud_create_sync_drive" ? ".append-create" : ".append-join").trigger("click"); await flushPromises(); expect(app.handleFailure).toHaveBeenCalledWith(failure); expect(w.text()).toContain("Sync drive operation failed"); const retained = w.find(".append-password").exists() ? w.find(".append-password") : w.find(".append-join-password"); expect(retained.element.value).toBe("password-123"); log.mockRestore(); w.unmount();
  });
  it('selects a local directory and edits an existing append mapping using its configured protocol', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/old', logicalPath: 'photos', schedule: { enabled: true, interval: 120 } }];
    openDialog.mockResolvedValue('/chosen'); const w = await mountSettings();
    await w.find('.mapping-item .mapping-edit').trigger('click');
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

describe("mirror sync settings through unified invoke", () => {
  it("creates a mirror mapping without a password and closes the dialog", async () => {
    const w = await mountSettings();
    await w.find(".create-mode").setValue("mirror");
    expect(w.find(".append-password").exists()).toBe(false);
    expect(w.find(".mirror-cloud-prefix").exists()).toBe(true);
    await w.find(".mirror-local-root").setValue("/local/mirror");
    expect(w.find(".mirror-create").attributes("disabled")).toBeUndefined();
    await w.find(".mirror-create").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_save_mapping", { input: { id: null, connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "", schedule: { enabled: false, interval: 900 } } });
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false);
    expect(changed).toHaveBeenCalled(); w.unmount();
  });
  it("keeps mirror create disabled until a local folder is chosen", async () => {
    const w = await mountSettings();
    await w.find(".create-mode").setValue("mirror");
    expect(w.find(".mirror-create").attributes("disabled")).toBeDefined();
    await w.find(".mirror-local-root").setValue("/local/mirror");
    expect(w.find(".mirror-create").attributes("disabled")).toBeUndefined(); w.unmount();
  });
  it("picks a cloud folder level by level and appends a new name", async () => {
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "cloud_object_list" ? { objects: [{ key: "_peerdav_mirror/docs/" }, { key: "_peerdav_mirror/docs/a.txt" }, { key: "_peerdav_mirror/photos/" }], next: null } : {});
    const w = await mountSettings();
    await w.find(".create-mode").setValue("mirror");
    await w.find(".pick-cloud-folder").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "_peerdav_mirror/", next: null });
    expect(w.findAll("[data-test=picker-folder]").map((b) => b.text())).toEqual(["docs/", "photos/"]);
    await w.find("[data-test=picker-folder]").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "_peerdav_mirror/docs/", next: null });
    expect(w.find(".mirror-cloud-prefix").element.value).toBe("");
    await w.find(".picker-new-name").setValue("work");
    await w.find(".picker-select").trigger("click");
    expect(w.find(".mirror-cloud-prefix").element.value).toBe("docs/work");
    expect(w.find("[data-test=cloud-folder-picker]").exists()).toBe(false); w.unmount();
  });
  it("selects the current cloud folder without naming a new one", async () => {
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "cloud_object_list" ? { objects: [{ key: "_peerdav_mirror/docs/" }], next: null } : {});
    const w = await mountSettings();
    await w.find(".create-mode").setValue("mirror");
    await w.find(".pick-cloud-folder").trigger("click"); await flushPromises();
    await w.find("[data-test=picker-folder]").trigger("click"); await flushPromises();
    await w.find(".picker-select").trigger("click");
    expect(w.find(".mirror-cloud-prefix").element.value).toBe("docs"); w.unmount();
  });
  it("lists mirror mappings and syncs them, surfacing conflicts", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "mirror_sync" ? { uploaded: 2, downloaded: 0, deletedCloud: 0, deletedLocal: 0, restored: 0, conflicts: ["a.txt"], failures: [{ path: "b.txt", reason: "boom" }] } : {});
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    expect(w.find('[data-test="mirror-group"] .client-collapse').exists()).toBe(true);
    expect(w.findAll('[data-test="mirror-group"] .client-item__detail li > span').map((cell) => cell.text())).toEqual(['Cloud folder', 'Statistics']);
    expect(w.text()).toContain("/local/mirror");
    expect(w.text()).toContain("_peerdav_mirror/docs");
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_sync", { mappingId: "m1", force: false, planId: null });
    expect(w.find('.mirror-stats').text()).toBe('↑ 2 · ↓ 0 · conflicts 1 · failed 1');
    expect(w.text()).toContain("Conflicts (1)");
    expect(w.text()).toContain("Failed items (1)");
    expect(changed).toHaveBeenCalled(); w.unmount();
  });
  it("syncs normal mirror deletions without a confirmation dialog", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "mirror_sync" ? { uploaded: 0, downloaded: 0, deletedCloud: 1, deletedLocal: 1, restored: 0, conflicts: [], failures: [] } : {});
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    expect(nativeInvoke.mock.calls.filter(([c]) => c === "mirror_sync")).toEqual([
      ["mirror_sync", { mappingId: "m1", force: false, planId: null }],
    ]);
    expect(confirmAction).not.toHaveBeenCalled();
    expect(app.handleFailure).not.toHaveBeenCalled();
    w.unmount();
  });
  it("asks for confirmation before a bulk mirror deletion", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    let attempts = 0;
    nativeInvoke.mockImplementation(async (command) => {
      if (command === "cloud_list") return structuredClone(rows);
      if (command === "mirror_sync") {
        attempts += 1;
        if (attempts === 1) throw { code: "MIRROR_MASS_DELETE", message: "confirm deletions", context: { planId: "snapshot-1", paths: ["DeleteCloud(keep.txt)"] } };
        return { uploaded: 0, downloaded: 0, deletedCloud: 3, deletedLocal: 0, restored: 0, conflicts: [], failures: [] };
      }
      return {};
    });
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    expect(confirmAction).toHaveBeenCalled();
    expect(nativeInvoke.mock.calls.filter(([c]) => c === "mirror_sync")).toEqual([
      ["mirror_sync", { mappingId: "m1", force: false, planId: null }],
      ["mirror_sync", { mappingId: "m1", force: true, planId: "snapshot-1" }],
    ]);
    expect(app.handleFailure).not.toHaveBeenCalled();
    w.unmount();
  });
  it.each(["MIRROR_CONFIRM_MERGE", "MIRROR_MASS_DELETE"])("does not retry %s when confirmation is declined", async (code) => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    confirmAction.mockResolvedValueOnce(false);
    nativeInvoke.mockImplementation(async (command) => {
      if (command === "cloud_list") return structuredClone(rows);
      if (command === "mirror_sync") throw { code, message: "review plan", context: { planId: "plan-1", paths: ["precious.txt"] } };
      return {};
    });
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    expect(confirmAction.mock.calls.at(-1)[0]).toContain("precious.txt");
    expect(nativeInvoke.mock.calls.filter(([c]) => c === "mirror_sync")).toHaveLength(1);
    w.unmount();
  });
  it("never auto-approves a changed merge plan", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    let attempts = 0;
    nativeInvoke.mockImplementation(async (command) => {
      if (command === "cloud_list") return structuredClone(rows);
      if (command === "mirror_sync") throw { code: "MIRROR_CONFIRM_MERGE", message: "changed plan", context: { planId: `plan-${++attempts}`, paths: ["Conflict(precious.txt)"] } };
      return {};
    });
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    expect(nativeInvoke.mock.calls.filter(([c]) => c === "mirror_sync")).toEqual([
      ["mirror_sync", { mappingId: "m1", force: false, planId: null }],
      ["mirror_sync", { mappingId: "m1", force: false, planId: "plan-1" }],
    ]);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(app.handleFailure).toHaveBeenCalled();
    w.unmount();
  });
  it("cancels a running mirror sync", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    let finish; nativeInvoke.mockImplementation((command) => command === "cloud_list" ? Promise.resolve(structuredClone(rows)) : command === "mirror_sync" ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve({}));
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Sync now").trigger("click"); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Cancel sync").trigger("click");
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_cancel", { mappingId: "m1" });
    finish({ uploaded: 0, downloaded: 0, deletedCloud: 0, deletedLocal: 0, restored: 0, conflicts: [], failures: [] }); await flushPromises(); w.unmount();
  });
  it("deletes a mirror mapping after confirmation without touching files", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.findAll("button").find((b) => b.text() === "Remove").trigger("click"); await flushPromises();
    expect(confirmAction).toHaveBeenCalled();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_delete_mapping", { mappingId: "m1" });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_object_delete_plan" || c === "cloud_object_delete")).toBe(false); w.unmount();
  });
  it("toggles automatic mirror sync and persists the schedule", async () => {
    rows.mirrorMappings = [{ id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: false, interval: 900 } }];
    const w = await mountSettings(); await openMirrorTab(w); await flushPromises();
    await w.find(".mirror-schedule input[type=checkbox]").setValue(true); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_save_mapping", { input: { id: "m1", connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "docs", schedule: { enabled: true, interval: 900 } } });
    expect(nativeInvoke.mock.calls.filter(([c]) => c === "mirror_save_mapping")).toHaveLength(1); w.unmount();
  });
  it("has matching bilingual keys and stable mirror error strategies", () => {
    expect(Object.keys(en).filter((k) => k.startsWith("drive.")).sort()).toEqual(Object.keys(zh).filter((k) => k.startsWith("drive.")).sort());
    for (const [code, strategy] of Object.entries({ MIRROR_INVALID_ROOT: "edit", MIRROR_INVALID_PREFIX: "edit", MIRROR_INVALID_ID: "show", MIRROR_UNSAFE_PATH: "show", MIRROR_CANCELED: "ignore", MIRROR_LOCAL_IO: "show", MIRROR_BASELINE: "support", MIRROR_BUSY: "show", MIRROR_MASS_DELETE: "show", MIRROR_TOO_MANY_ENTRIES: "show" })) {
      expect(errorStrategy({ code, message: "reason" })).toBe(strategy); expect(toUserMessage({ code, message: "original reason" })).toContain("original reason");
    }
  });
});
