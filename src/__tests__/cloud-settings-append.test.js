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
  it.each(['encrypted', 'authenticated_plain', 'mirror'])('defaults the %s remote folder from the local name without overwriting edits', async (mode) => {
    const w = await mountSettings(); await w.find('.create-mode').setValue(mode);
    const local = mode === 'mirror' ? '.mirror-local-root' : '.new-drive-local';
    const remote = mode === 'mirror' ? '.mirror-cloud-prefix' : '.new-drive-folder';
    const fields = w.findAll('.drive-create input').map((entry) => entry.element);
    expect(fields.indexOf(w.find(local).element)).toBeLessThan(fields.indexOf(w.find(remote).element));
    await w.find(local).setValue('/local/Documents');
    expect(w.find(remote).element.value).toBe('Documents');
    await w.find(local).setValue('/local/Photos');
    expect(w.find(remote).element.value).toBe('Photos');
    await w.find(remote).setValue('custom'); await w.find(local).setValue('/local/Other');
    expect(w.find(remote).element.value).toBe('custom');
    w.unmount();
  });
  it.each(['encrypted', 'authenticated_plain', 'mirror'])('lets %s drives select a bucket without changing the browser bucket', async (mode) => {
    rows.connections[0] = { id: 'c1', bucket: 'main', buckets: ['main', 'photos'] };
    const w = await mountSettings(); await w.setProps({ bucket: 'main' });
    await w.find('.create-mode').setValue(mode);
    expect(w.find('.create-bucket').element.value).toBe('main');
    await w.find('.create-bucket').setValue('photos');
    if (mode === 'mirror') {
      await w.find('.mirror-local-root').setValue('/local/mirror');
      await w.find('.mirror-create').trigger('click');
    } else {
      await w.find('.new-drive-local').setValue('/local/docs'); await fillPasswords(w);
      await w.find('.append-create').trigger('click');
    }
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith(mode === 'mirror' ? 'mirror_save_mapping' : 'cloud_create_sync_drive', { input: expect.objectContaining({ bucket: 'photos' }) });
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_object_select_bucket')).toBe(false);
    w.unmount();
  });
  it.each([
    ['', 'Enter a password', 0], ['Ab1!x', 'Blocked: at least 6 characters', 0],
    ['123456', 'Unsafe', 1], ['abcdef', 'Unsafe', 1], ['ABCDEF', 'Unsafe', 1],
    ['abcdefghi', 'Unsafe', 1], ['abc123', 'Low', 2], ['Abcdef', 'Unsafe', 1],
    ['Abc123', 'Medium-low', 2], ['Abcd1234', 'Medium-low', 2],
    ['Ab1!xy', 'Medium-high', 3], ['Abcd12!?', 'Medium-high', 3],
    ['Abc123456', 'High', 4], ['abc12345!', 'High', 4], ['Abcdefgh!', 'High', 4],
    ['abcd12345', 'Low', 2], ['abc123   ', 'Low', 2], ['合同记录资料', 'Unsafe', 1],
    ['😀😀😀', 'Blocked: at least 6 characters', 0], ['😀😀😀😀😀😀', 'Unsafe', 1],
  ])('rates creation password %j as %s', async (value, label, score) => {
    const w = await mountSettings(); await w.find('.new-drive-local').setValue('/local');
    await w.find('.append-password').setValue(value); await w.find('.append-confirmation').setValue(value);
    expect(w.find('.password-strength span').text()).toBe(`Password complexity：${label}`);
    expect(w.find('.password-strength meter').element.value).toBe(score);
    expect(w.find('.append-create').attributes('disabled') !== undefined).toBe(Array.from(value).length < 6);
    if (Array.from(value).length < 6) { await w.find('form').trigger('submit'); expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_create_sync_drive')).toBe(false); }
    w.unmount();
  });
  it('localizes the indicator and hides it for mirrors and connecting existing drives', async () => {
    setLocale('zh-CN'); const w = await mountSettings(); await w.find('.append-password').setValue('Abc123');
    expect(w.find('.password-strength span').text()).toBe('密码复杂度：中低');
    await w.find('.create-mode').setValue('authenticated_plain'); expect(w.find('.password-strength').exists()).toBe(true);
    await w.find('.create-mode').setValue('mirror'); expect(w.find('.password-strength').exists()).toBe(false);
    await w.find('.cancel-create-drive').trigger('click'); await w.find('.add-menu-trigger').trigger('click'); await w.find('.add-connect-drive').trigger('click'); await flushPromises();
    await w.find('.append-candidate').setValue('one'); await w.find('.append-join-password').setValue('p'); await fillLinkLocalRoot(w);
    expect(w.find('.password-strength').exists()).toBe(false); expect(w.find('.append-join').attributes('disabled')).toBeUndefined();
    await w.find('.append-join').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_append_join', { input: expect.objectContaining({ password: 'p' }) }); w.unmount();
  });
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
    expect(w.text()).not.toMatch(/CAS|Append V2|Other drive|append-repo/); expect(w.find('.drive-list').exists()).toBe(false); expect(w.text()).not.toContain("Legacy drive"); w.unmount();
  });
  it('keeps focused repository selection without the redundant drive list', async () => {
    const w = await mountSettings(); await w.setProps({ focusId: 'append-repo' });
    expect(w.find('.drive-list').exists()).toBe(false);
    await openMappingDialog(w);
    expect(w.find('.mapping-repository').element.value).toBe('append-repo');
    await w.setProps({ focusId: 'cas-repo' });
    expect(w.find('.drive-list').exists()).toBe(false); w.unmount();
  });
  it.each(["encrypted", "authenticated_plain"])("creates %s and binds the first local folder in one step", async (mode) => {
    const w = await mountSettings(); await w.find(".create-mode").setValue(mode); await w.find(".new-drive-local").setValue("/local/Documents");
    expect(w.find(".append-create").attributes("disabled")).toBeDefined();
    if (mode === "authenticated_plain") expect(w.find(".create-mode-hint").text()).toContain("stored in plaintext");
    await fillPasswords(w); expect(w.find(".append-create").attributes("disabled")).toBeUndefined();
    await w.find(".append-create").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_create_sync_drive", { input: { connectionId: "c1", mode, password: "password-123", passwordConfirmation: "password-123" } });
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "new-repo", localRoot: "/local/Documents", logicalPath: "Documents", schedule: { enabled: false, interval: 900 } }, selectedRoot: "Documents" } });
    await flushPromises();
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('.append-state').text()).toBe('Sync drive saved.');
    expect(changed).toHaveBeenCalledTimes(1);
    expect(w.find('[data-test=create-drive-dialog]').exists()).toBe(false); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it('binds an empty new drive directly and preserves the selected schedule', async () => {
    const original = nativeInvoke.getMockImplementation();
    nativeInvoke.mockImplementation(async (command, args) => {
      if (command === 'cloud_prepare_mapping_bind') { const item = { ...args.input.mapping, id: 'scheduled' }; rows.mappings.push(item); return { mapping: item, runId: 1, planId: null, entries: [] }; }
      return original(command, args);
    });
    const w = await mountSettings(); await w.setProps({ bucket: 'photos' });
    await w.find('.new-drive-local').setValue('/local/Documents'); await fillPasswords(w);
    await w.find('.create-schedule input[type=checkbox]').setValue(true);
    await w.find('.create-schedule input[type=number]').setValue('120');
    await w.find('.append-create').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_create_sync_drive', { input: { connectionId: 'c1', bucket: 'photos', mode: 'encrypted', password: 'password-123', passwordConfirmation: 'password-123' } });
    expect(rows.mappings[0].schedule).toEqual({ enabled: true, interval: 120 });
    expect(confirmAction).not.toHaveBeenCalled();
    expect(nativeInvoke.mock.calls.some(([command]) => command === 'cloud_preview' || command === 'cloud_sync')).toBe(false);
    w.unmount();
  });
  it('scopes discovery and joining to the selected bucket', async () => {
    const w = await appendSettings(); await w.setProps({ bucket: 'photos' });
    await w.find('.append-discover').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_append_discover', { connectionId: 'c1', bucket: 'photos' });
    await w.find('.append-candidate').setValue('one'); await fillPasswords(w); await fillLinkLocalRoot(w);
    await w.find('.append-join').trigger('click'); await flushPromises();
    expect(nativeInvoke.mock.calls.find(([command]) => command === 'cloud_append_join')[1].input.bucket).toBe('photos'); w.unmount();
  });
  it("connects with a password without security-code prompts and clears it when selection changes", async () => {
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: ["docs"], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_discover", { connectionId: "c1" }); expect(w.find(".append-candidate").element.value).toBe(""); expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w);
    expect(w.find('.advanced-security').exists()).toBe(false); expect(w.find('.append-fingerprint').exists()).toBe(false); expect(w.find('.append-tofu').exists()).toBe(false);
    expect(w.find(".append-join").attributes("disabled")).toBeUndefined();
    await w.find(".append-candidate").setValue("two"); expect(w.find('.append-join-password').element.value).toBe(''); expect(w.find('.append-join').attributes('disabled')).toBeDefined();
    await fillPasswords(w); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_append_join", { input: { connectionId: "c1", anchorKey: "two", anchorFingerprint: "fp-two", repositoryId: "repo-two", mode: "authenticated_plain", acceptTofu: true, trustedFingerprint: null, password: "password-123" } });
    await flushPromises();
    expect(app.handleFailure.mock.calls).toEqual([]);
    expect(w.find('.append-state').text()).toBe('Sync drive saved.');
    expect(changed).toHaveBeenCalledTimes(1);
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "joined-repo", localRoot: "/local/Documents", logicalPath: "docs", schedule: { enabled: false, interval: 900 } }, selectedRoot: "docs" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("asks for a root when the joined drive has several and binds the chosen one", async () => {
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: ["docs", "photos"], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); await w.find(".append-join").trigger("click"); await flushPromises();
    // 多根目录：join 成功但尚未绑定，等待用户选择。
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    expect(w.find(".link-root").exists()).toBe(true);
    await w.find(".link-root").setValue("photos"); await w.find(".append-bind").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "joined-repo", localRoot: "/local/Documents", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(w.find('.append-state').text()).toBe('Sync drive saved.'); expect(changed).toHaveBeenCalledTimes(1); w.unmount();
  });
  it("asks for a new root name when the joined drive is empty", async () => {
    rows.appendRepositories.push({ connectionId: "c1", protocol: "append_v2", binding: { repoId: "joined-repo", rootCandidates: [], appendMode: "encrypted" } });
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); await w.find(".append-join").trigger("click"); await flushPromises();
    expect(w.find(".link-new-root").exists()).toBe(true);
    await w.find(".link-new-root").setValue("fresh-root"); await w.find(".append-bind").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: expect.objectContaining({ selectedRoot: "fresh-root", mapping: expect.objectContaining({ logicalPath: "fresh-root" }) }) }); w.unmount();
  });
  it("keeps the join button disabled until a local folder is chosen", async () => {
    const w = await appendSettings(); await w.find(".append-discover").trigger("click"); await flushPromises();
    await w.find(".append-candidate").setValue("one"); await fillPasswords(w);
    expect(w.find(".append-join").attributes("disabled")).toBeDefined();
    await fillLinkLocalRoot(w); expect(w.find(".append-join").attributes("disabled")).toBeUndefined(); w.unmount();
  });
  it("shows loading through joining and saving, submits once, and closes only after save", async () => {
    rows.appendRepositories.push({ connectionId: 'c1', protocol: 'append_v2', binding: { repoId: 'joined-repo', rootCandidates: ['docs'], appendMode: 'encrypted' } });
    const w = await appendSettings(); await w.find('.append-candidate').setValue('one'); await fillPasswords(w); await fillLinkLocalRoot(w);
    const original = nativeInvoke.getMockImplementation(); let joinDone; let saveDone;
    nativeInvoke.mockImplementation((command, args) => command === 'cloud_append_join' ? new Promise(resolve => { joinDone = resolve; }) : command === 'cloud_prepare_mapping_bind' ? new Promise(resolve => { saveDone = resolve; }) : original(command, args));
    await w.find('.append-join').trigger('click');
    expect(w.find('.append-join').attributes('loading')).toBe('true'); expect(w.find('.cancel-connect-drive').attributes('disabled')).toBeDefined();
    await w.find('.append-join').trigger('click'); expect(nativeInvoke.mock.calls.filter(([c]) => c === 'cloud_append_join')).toHaveLength(1);
    joinDone({ binding: { repoId: 'joined-repo' } }); await flushPromises();
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(true); expect(w.find('.append-join').attributes('loading')).toBe('true');
    saveDone({}); await flushPromises();
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(false);
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_preview' || c === 'cloud_sync')).toBe(false); w.unmount();
  });
  it.each([false, true])('confirms populated-folder plans before binding; delete=%s', async (risky) => {
    rows.appendRepositories.push({ connectionId: 'c1', protocol: 'append_v2', binding: { repoId: 'joined-repo', rootCandidates: ['docs'], appendMode: 'encrypted' } });
    const w = await appendSettings(); await w.find('.append-candidate').setValue('one'); await fillPasswords(w); await fillLinkLocalRoot(w);
    const original = nativeInvoke.getMockImplementation(); let confirm;
    confirmAction.mockImplementationOnce(() => new Promise(resolve => { confirm = resolve; }));
    nativeInvoke.mockImplementation((command, args) => {
      if (command === 'cloud_prepare_mapping_bind') return Promise.resolve({ planId: 'initial', entries: [{ path: 'remote.txt', action: 'download' }, ...(risky ? [{ path: 'removed.txt', action: 'delete' }] : [])] });
      if (command === 'cloud_confirm_mapping_bind') { rows.mappings.push({ id: 'linked', connectionId: 'c1' }); return Promise.resolve({ mapping: { id: 'linked' }, runId: 1 }); }
      return original(command, args);
    });
    await w.find('.append-join').trigger('click'); await flushPromises();
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(true);
    expect(rows.mappings).toHaveLength(0);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(confirmAction.mock.calls[0][2].danger).toBe(risky);
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_confirm_mapping_bind')).toBe(false);
    confirm(true); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_confirm_mapping_bind', { planId: 'initial', confirmed: true });
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(false);
    w.unmount();
  });
  it('cancels a populated-folder binding without publishing or starting it', async () => {
    rows.appendRepositories.push({ connectionId: 'c1', protocol: 'append_v2', binding: { repoId: 'joined-repo', rootCandidates: ['docs'], appendMode: 'encrypted' } });
    const w = await appendSettings(); await w.find('.append-candidate').setValue('one'); await fillPasswords(w); await fillLinkLocalRoot(w);
    const original = nativeInvoke.getMockImplementation();
    nativeInvoke.mockImplementation((command, args) => command === 'cloud_prepare_mapping_bind' ? Promise.resolve({ planId: 'cancelled-plan', entries: [{ path: 'file.txt', action: 'upload' }] }) : original(command, args));
    confirmAction.mockResolvedValueOnce(false);
    await w.find('.append-join').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_cancel_mapping_bind', { planId: 'cancelled-plan' });
    expect(nativeInvoke.mock.calls.some(([command]) => command === 'cloud_confirm_mapping_bind' || command === 'cloud_sync' || command === 'cloud_append_save_mapping')).toBe(false);
    expect(rows.mappings).toHaveLength(0);
    expect(w.find('[data-test=connect-drive-dialog]').exists()).toBe(true);
    w.unmount();
  });
  it("does not preselect any authenticated root and routes append mapping separately", async () => {
    const w = await mountSettings(); await openMappingDialog(w); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local'); expect(w.find(".append-root").element.value).toBe(""); await saveMappingButton(w).trigger("click"); expect(app.setError).toHaveBeenCalledWith("cloudAppend.rootRequired"); expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_append_save_mapping")).toBe(false);
    await w.find(".append-root").setValue("photos"); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: { mappingId: null, mapping: { connectionId: "c1", repositoryId: "append-repo", localRoot: "/local", logicalPath: "photos", schedule: { enabled: false, interval: 900 } }, selectedRoot: "photos" } });
    expect(nativeInvoke.mock.calls.some(([c]) => c === "cloud_save_mapping" || c === "cloud_test_connection")).toBe(false); w.unmount();
  });
  it("allows an explicitly named root only for an empty append repository", async () => {
    rows.appendRepositories[0].binding.rootCandidates = []; const w = await mountSettings(); await openMappingDialog(w); await w.find('.mapping-repository').setValue('append-repo'); await w.find('.mapping-local').setValue('/local');
    await w.find('.mapping-remote').setValue('new-root'); await saveMappingButton(w).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_prepare_mapping_bind", { input: expect.objectContaining({ selectedRoot: "new-root", mapping: expect.objectContaining({ logicalPath: "new-root" }) }) }); w.unmount();
  });
  it('timestamps the preview and clears stale failure details on retry', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    rows.statuses = [{ mappingId: 'a1', phase: 'failed', detail: 'scan canceled' }];
    const w = await mountSettings();
    expect(w.find('.mapping-status').text()).not.toContain('scan canceled');
    nativeInvoke.mockResolvedValueOnce({ previewId: 'empty', summary: { uploads: 0 } });
    await w.find('.mapping-preview').trigger('click'); await flushPromises();
    expect(w.find('.cloud-preview time').attributes('datetime')).toBeTruthy();
    expect(w.find('.mapping-status').text()).not.toContain('Failed');
    w.unmount();
  });
  it('opens and saves the mirror editor using the existing mapping identity', async () => {
    rows.connections[0] = { id: 'c1', bucket: 'main', buckets: ['main', 'photos'] };
    rows.mirrorMappings = [{ id: 'm1', connectionId: 'c1', bucket: 'photos', localRoot: '/local/photos', cloudPrefix: 'pictures', schedule: { enabled: true, interval: 120 } }];
    const w = await mountSettings();
    await w.find('.mirror-edit').trigger('click');
    expect(w.find('.mirror-local-root').element.value).toBe('/local/photos');
    expect(w.find('.mirror-cloud-prefix').element.value).toBe('pictures');
    await w.find('.mirror-local-root').setValue('/new/photos');
    await w.find('.mirror-create').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('mirror_save_mapping', { input: expect.objectContaining({ id: 'm1', bucket: 'photos', localRoot: '/new/photos', cloudPrefix: 'pictures' }) });
    w.unmount();
  });
  it.each(['encrypted', 'authenticated_plain'])('saves inline automatic sync settings for %s', async (mode) => {
    rows.appendRepositories[0].binding.appendMode = mode;
    rows.mappings = [{ id: 'a1', connectionId: 'c1', protocol: 'append_v2', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos', schedule: { enabled: false, interval: 900 } }];
    const w = await mountSettings();
    await w.find('.mapping-schedule input[type=checkbox]').setValue(true); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_append_save_mapping', { input: expect.objectContaining({ mappingId: 'a1', mapping: expect.objectContaining({ schedule: { enabled: true, interval: 900 } }) }) });
    w.unmount();
  });
  it('persists the inline interval and clamps it to the supported minimum', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', protocol: 'append_v2', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos', schedule: { enabled: true, interval: 900 } }];
    const w = await mountSettings();
    await w.find('.mapping-schedule input[type=number]').setValue('30'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_append_save_mapping', { input: expect.objectContaining({ mappingId: 'a1', mapping: expect.objectContaining({ schedule: { enabled: true, interval: 60 } }) }) });
    w.unmount();
  });
  it('shows preview loading and omits confirmation for a zero-change plan', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos' }];
    const w = await mountSettings();
    let finish;
    nativeInvoke.mockImplementation((command) => command === 'cloud_preview' ? new Promise((resolve) => { finish = resolve; }) : command === 'cloud_list' ? Promise.resolve(structuredClone(rows)) : Promise.resolve([]));
    await w.find('.mapping-preview').trigger('click');
    expect(w.find('.mapping-preview').attributes('loading')).toBe('true');
    finish({ previewId: 'empty', summary: { uploads: 0, downloads: 0, deletes: 0, conflicts: 0 } }); await flushPromises();
    expect(w.find('.cloud-preview').text()).toContain('Already up to date');
    expect(w.find('.cloud-preview button').exists()).toBe(false);
    expect(w.find('.mapping-preview').attributes('loading')).toBe('false');
    w.unmount();
  });
  it('disables recovery writes while the mapping is syncing', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos' }];
    rows.statuses = [{ mappingId: 'a1', phase: 'uploading', runId: 1 }];
    const original = nativeInvoke.getMockImplementation();
    nativeInvoke.mockImplementation((command, args) => command === 'cloud_recovery_records' ? Promise.resolve([{ id: 'record', relativePath: 'old.txt', phase: 'completed' }]) : original(command, args));
    const w = await mountSettings();
    await w.find('.mapping-recovery').trigger('click'); await flushPromises();
    expect(w.find('.recovery-list button').attributes('disabled')).toBeDefined();
    await w.find('.recovery-list button').trigger('click'); await flushPromises();
    expect(nativeInvoke.mock.calls.some(([command]) => command === 'cloud_restore')).toBe(false);
    w.unmount();
  });
  it.each([false, true])('discovers recovery records in the background (records=%s)', async (hasRecords) => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos' }];
    let finish;
    nativeInvoke.mockImplementation((command) => command === 'cloud_list' ? Promise.resolve(structuredClone(rows)) : command === 'cloud_recovery_records' ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve({}));
    const w = await mountSettings();
    expect(w.find('.mapping-recovery').exists()).toBe(false);
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_recovery_records', { mappingId: 'a1' });
    finish(hasRecords ? [{ id: 'record', relativePath: 'old.txt', phase: 'completed' }] : []); await flushPromises();
    expect(w.find('.mapping-recovery').exists()).toBe(hasRecords);
    if (hasRecords) { await w.find('.mapping-recovery').trigger('click'); expect(w.find('.recovery-list').text()).toContain('old.txt'); }
    w.unmount();
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
  it.each([false, true])('syncs directly only when the fresh preview is safe (risky=%s)', async (risky) => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos', schedule: {} }];
    const w = await mountSettings();
    expect(w.find('.mapping-cancel').exists()).toBe(false);
    nativeInvoke.mockResolvedValueOnce({ previewId: 'fresh', summary: { uploads: 1, deletes: risky ? 1 : 0, conflicts: 0 } });
    await w.find('.mapping-sync').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_preview', { mappingId: 'a1' });
    if (risky) {
      expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_sync')).toBe(false);
      await w.find('.cloud-preview button').trigger('click'); await flushPromises();
    }
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_sync', { mappingId: 'a1', previewId: 'fresh' });
    w.unmount();
  });
  it('shows a notice instead of an error for a duplicate manual sync', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    const w = await mountSettings();
    nativeInvoke.mockRejectedValueOnce({ code: 'SYNC_ALREADY_RUNNING', message: 'Already running' });
    await w.find('.mapping-sync').trigger('click'); await flushPromises();
    expect(app.setNotice).toHaveBeenCalledWith('cloud.syncAlreadyRunning');
    expect(app.handleFailure).not.toHaveBeenCalled();
    expect(nativeInvoke.mock.calls.some(([command]) => command === 'cloud_sync')).toBe(false);
    w.unmount();
  });
  it('does not sync an empty plan and clears loading after a preview error', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    const w = await mountSettings();
    nativeInvoke.mockResolvedValueOnce({ previewId: 'empty', summary: { uploads: 0, downloads: 0, deletes: 0, conflicts: 0 } });
    await w.find('.mapping-sync').trigger('click'); await flushPromises();
    expect(w.find('.cloud-preview button').exists()).toBe(false);
    expect(nativeInvoke.mock.calls.some(([command]) => command === 'cloud_sync')).toBe(false);
    const error = { code: 'CLOUD_SCAN_FAILED', message: 'Folder unavailable' };
    nativeInvoke.mockRejectedValueOnce(error);
    await w.find('.mapping-preview').trigger('click'); await flushPromises();
    expect(w.find('.mapping-preview').attributes('loading')).toBe('false');
    expect(w.find('.cloud-preview').exists()).toBe(false);
    expect(app.handleFailure).toHaveBeenCalledWith(expect.objectContaining(error));
    w.unmount();
  });
  it('ignores recovery results after switching connections', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    let finish;
    nativeInvoke.mockImplementation((command) => command === 'cloud_list' ? Promise.resolve(structuredClone(rows)) : command === 'cloud_recovery_records' ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve({}));
    const w = await mountSettings();
    await w.setProps({ connectionId: 'c2' }); await flushPromises();
    finish([{ id: 'stale', relativePath: 'old.txt' }]); await flushPromises();
    expect(w.find('.mapping-recovery').exists()).toBe(false);
    expect(w.find('.recovery-list').exists()).toBe(false);
    w.unmount();
  });
  it.each([false, true])('keeps sync loading through completion or failure (failure=%s)', async (failure) => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    const w = await mountSettings();
    let finish, reject;
    nativeInvoke.mockImplementation((command) => command === 'cloud_preview' ? Promise.resolve({ previewId: 'plan', summary: { uploads: 1 } }) : command === 'cloud_sync' ? new Promise((resolve, fail) => { finish = resolve; reject = fail; }) : command === 'cloud_list' ? Promise.resolve(structuredClone(rows)) : Promise.resolve([]));
    await w.find('.mapping-sync').trigger('click'); await flushPromises();
    expect(w.find('.mapping-sync').attributes('loading')).toBe('true');
    const error = { code: 'CLOUD_SYNC_FAILED', message: 'Sync failed' };
    if (failure) reject(error); else finish({});
    await flushPromises();
    expect(w.find('.mapping-sync').attributes('loading')).toBe('false');
    if (failure) expect(app.handleFailure).toHaveBeenCalledWith(expect.objectContaining(error));
    w.unmount();
  });
  it.each(['encrypted', 'authenticated_plain'])('uses a non-action icon for %s drives', async (mode) => {
    rows.appendRepositories[0].binding.appendMode = mode;
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local' }];
    rows.mirrorMappings = [{ id: 'm1', connectionId: 'c1', localRoot: '/mirror' }];
    const w = await mountSettings();
    expect(w.find(`[data-test=${mode === 'encrypted' ? 'encrypted' : 'plain'}-drive-icon]`).exists()).toBe(true);
    expect(w.find('[data-test=mirror-drive-icon]').exists()).toBe(true);
    expect(w.find('.mapping-item .client-item__icon').attributes('aria-label')).toBe(mode === 'encrypted' ? en['cloud.encrypted'] : en['drive.modeAuthenticatedPlain']);
    w.unmount();
  });
  it('offers stop only for an active sync', async () => {
    rows.mappings = [{ id: 'a1', connectionId: 'c1', repositoryId: 'append-repo', localRoot: '/local', logicalPath: 'photos', schedule: {} }];
    rows.statuses = [{ mappingId: 'a1', phase: 'uploading' }];
    const w = await mountSettings();
    expect(w.find('.mapping-sync').attributes('loading')).toBe('true');
    expect(w.find('.mapping-sync').attributes('disabled')).toBeDefined();
    expect(w.find('.mapping-preview').attributes('disabled')).toBeDefined();
    await w.find('.mapping-cancel').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_cancel', { mappingId: 'a1' }); w.unmount();
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
    if (command === "cloud_append_join") { await w.find(".append-discover").trigger("click"); await flushPromises(); await w.find(".append-candidate").setValue("one"); await fillPasswords(w); await fillLinkLocalRoot(w); }
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
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_save_mapping", { input: { id: null, connectionId: "c1", localRoot: "/local/mirror", cloudPrefix: "mirror", schedule: { enabled: false, interval: 900 } } });
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
  it('pages the selected mirror bucket without rebinding the file browser', async () => {
    rows.connections[0] = { id: 'c1', bucket: 'main', buckets: ['main', 'photos'] };
    const original = nativeInvoke.getMockImplementation();
    nativeInvoke.mockImplementation(async (command, args) => command === 'mirror_folders' ? {
      objects: [{ key: args.next ? '_peerdav_mirror/later/a.txt' : '_peerdav_mirror/first/a.txt' }], next: args.next ? null : 'page2'
    } : original(command, args));
    const w = await mountSettings(); await w.find('.create-mode').setValue('mirror');
    await w.find('.create-bucket').setValue('photos');
    await w.find('.pick-cloud-folder').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('mirror_folders', { connectionId: 'c1', bucket: 'photos', prefix: '', next: 'page2' });
    expect(w.findAll('[data-test=picker-folder]').map((b) => b.text())).toEqual(['first/', 'later/']);
    expect(nativeInvoke.mock.calls.some(([c]) => c === 'cloud_object_select_bucket')).toBe(false);
    await w.find('.create-bucket').setValue('main');
    expect(w.find('[data-test=cloud-folder-picker]').exists()).toBe(false); w.unmount();
  });
  it("picks a cloud folder level by level and appends a new name", async () => {
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "mirror_folders" ? { objects: [{ key: "_peerdav_mirror/docs/" }, { key: "_peerdav_mirror/docs/a.txt" }, { key: "_peerdav_mirror/photos/" }], next: null } : {});
    const w = await mountSettings();
    await w.find(".create-mode").setValue("mirror");
    await w.find(".pick-cloud-folder").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_folders", { connectionId: "c1", prefix: "", next: null });
    expect(w.findAll("[data-test=picker-folder]").map((b) => b.text())).toEqual(["docs/", "photos/"]);
    await w.find("[data-test=picker-folder]").trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("mirror_folders", { connectionId: "c1", prefix: "docs/", next: null });
    expect(w.find(".mirror-cloud-prefix").element.value).toBe("");
    await w.find(".picker-new-name").setValue("work");
    await w.find(".picker-select").trigger("click");
    expect(w.find(".mirror-cloud-prefix").element.value).toBe("docs/work");
    expect(w.find("[data-test=cloud-folder-picker]").exists()).toBe(false); w.unmount();
  });
  it("selects the current cloud folder without naming a new one", async () => {
    nativeInvoke.mockImplementation(async (command) => command === "cloud_list" ? structuredClone(rows) : command === "mirror_folders" ? { objects: [{ key: "_peerdav_mirror/docs/" }], next: null } : {});
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
