import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { cloud, confirmAction, openDialog, listen, app } = vi.hoisted(() => ({
  cloud: {
    list: vi.fn(), saveConnection: vi.fn(), testConnection: vi.fn(), deleteConnection: vi.fn(),
    createRepository: vi.fn(), joinRepository: vi.fn(), saveMapping: vi.fn(), deleteMapping: vi.fn(),
    preview: vi.fn(), sync: vi.fn(), cancel: vi.fn(), recoveryRecords: vi.fn(), restore: vi.fn(),
    probeBuckets: vi.fn(),
    connectionCredentials: vi.fn(),
  },
  confirmAction: vi.fn(), openDialog: vi.fn(), listen: vi.fn(),
  app: { handleFailure: vi.fn(), setNotice: vi.fn(), setError: vi.fn() },
}));
vi.mock("../lib/cloud", async (importOriginal) => { const actual = await importOriginal(); return { ...actual, cloud }; });
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("../lib/confirm", () => ({ confirmAction }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openDialog }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { setLocale } from "../lib/i18n";
import CloudSettings from "../components/CloudSettings.vue";

const inputStub = defineComponent({ props: ["modelValue", "type", "autocomplete"], emits: ["update:modelValue"], setup(props, { emit }) { return () => h("input", { value: props.modelValue, type: props.type || "text", autocomplete: props.autocomplete, onInput: (e) => emit("update:modelValue", e.target.value) }); } });
const buttonStub = defineComponent({ inheritAttrs: false, setup(_, { slots, attrs }) { return () => h("button", attrs, slots.default?.()); } });
const selectStub = defineComponent({ name: "SelectStub", props: ["modelValue"], emits: ["update:modelValue"], setup(props, { emit, slots }) { return () => h("select", { value: props.modelValue, onChange: (e) => emit("update:modelValue", e.target.value) }, slots.default?.()); } });
const optionStub = defineComponent({ props: ["value", "label"], setup(props) { return () => h("option", { value: props.value }, props.label); } });
const switchStub = defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(props, { emit }) { return () => h("input", { type: "checkbox", checked: props.modelValue, onChange: (e) => emit("update:modelValue", e.target.checked) }); } });

const rows = { connections: [{ id: "c1", name: "Primary", proxy: { url: "" } }], repositories: [{ id: "r1", name: "Repo", encrypted: true }], mappings: [{ id: "m1", localRoot: "/old", logicalPath: "docs", schedule: {} }], statuses: [], capabilities: { safeApply: true } };
let progressHandler;
async function mountCloud(data = rows) {
  cloud.list.mockResolvedValue(data);
  listen.mockImplementation(async (_, handler) => { progressHandler = handler; return vi.fn(); });
  const wrapper = mount(CloudSettings, { global: { stubs: { "el-input": inputStub, "el-button": buttonStub, "el-select": selectStub, "el-option": optionStub, "el-switch": switchStub } } });
  await flushPromises();
  return wrapper;
}

beforeEach(() => { setLocale("zh-CN"); Object.values(cloud).forEach((fn) => fn.mockReset()); confirmAction.mockReset(); confirmAction.mockResolvedValue(true); openDialog.mockReset(); app.handleFailure.mockReset(); app.setNotice.mockReset(); app.setError.mockReset(); progressHandler = undefined; cloud.connectionCredentials.mockResolvedValue({ accessKey: "", secretKey: "", sessionToken: null }); });

describe("CloudSettings", () => {
  it("defaults to a multi-drive list without connection or sync forms", async () => {
    const wrapper = await mountCloud();
    expect(wrapper.text()).toContain('Primary'); expect(wrapper.findAll('input')).toHaveLength(0);
    expect(wrapper.findAll('.connection-editor')).toHaveLength(0); expect(wrapper.text()).not.toMatch(/CAS|Append|Repo/);
    await wrapper.find('.cloud-browse-entry').trigger('click'); expect(window.location.hash).toBe('#/cloud/c1');
    expect(cloud.sync).not.toHaveBeenCalled(); wrapper.unmount(); window.history.replaceState(null, '', window.location.pathname);
  });
  it("opens a minimal add form with collapsed advanced settings and HTTP disabled", async () => {
    const wrapper = await mountCloud(); await wrapper.find('.connection-add').trigger('click');
    expect(wrapper.find('.connection-advanced').attributes('open')).toBeUndefined();
    expect(wrapper.find('.connection-editor').findAll('label').filter((l) => !l.element.closest('details'))).toHaveLength(5);
    expect(wrapper.findAll('input[type="checkbox"]').every((i) => !i.element.checked)).toBe(true); wrapper.unmount();
  });
  it("infers Qiniu into the service endpoint, probes buckets, and keeps HTTP opt-in", async () => {
    cloud.probeBuckets.mockResolvedValue([{ name: 'peerdav' }]);
    const wrapper = await mountCloud(); await wrapper.find('.connection-add').trigger('click');
    await wrapper.find('.connection-name input').setValue('New'); await wrapper.find('.connection-url input').setValue('http://peerdav.s3.cn-east-1.qiniucs.com');
    await wrapper.find('.connection-access-key input').setValue('AK'); await wrapper.find('.connection-secret-key input').setValue('SK');
    await wrapper.find('.connection-probe-run').trigger('click'); await flushPromises();
    expect(cloud.probeBuckets).toHaveBeenCalledWith(expect.objectContaining({ endpoint: 'http://s3.cn-east-1.qiniucs.com', accessKey: 'AK', secretKey: 'SK', region: 'cn-east-1', allowInsecureHttp: false }));
    await wrapper.find('.connection-save').trigger('click'); await flushPromises();
    expect(cloud.saveConnection).toHaveBeenCalledWith(expect.objectContaining({ name: 'New', endpoint: 'http://s3.cn-east-1.qiniucs.com', bucket: 'peerdav', region: 'cn-east-1', pathStyle: true, allowInsecureHttp: false }));
    // 弹窗用 v-show 隐藏叠加层而非卸载内容；离开过渡需 jsdom 泵送 rAF 才收敛，故先 settle 再判定已关闭。
    await flushPromises();
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    expect(/display:\s*none/.test(wrapper.find('.el-overlay').attributes('style') ?? '')).toBe(true); wrapper.unmount();
  });
  it("preserves edited values and credentials when URL is unchanged", async () => {
    const wrapper = await mountCloud({ ...rows, connections: [{ id: 'c1', name: 'Primary', endpoint: 'https://custom.example', bucket: 'old', region: 'custom', prefix: 'keep', pathStyle: true, proxy: { mode: 'custom', url: 'http://proxy', username: 'secret-user', password: 'secret-pass' } }] });
    await wrapper.find('.connection-edit').trigger('click'); expect(wrapper.findAll('input').some((i) => ['secret-user', 'secret-pass'].includes(i.element.value))).toBe(false);
    await wrapper.find('.connection-name input').setValue('Renamed'); await wrapper.find('.connection-save').trigger('click'); await flushPromises();
    expect(cloud.saveConnection).toHaveBeenCalledWith(expect.objectContaining({ id: 'c1', name: 'Renamed', endpoint: 'https://custom.example', bucket: 'old', region: 'custom', prefix: 'keep', pathStyle: true, accessKey: undefined, secretKey: undefined, sessionToken: undefined })); wrapper.unmount();
  });
  it("backfills saved credentials and the multi-bucket selection when editing", async () => {
    cloud.connectionCredentials.mockResolvedValue({ accessKey: "AK", secretKey: "SK", sessionToken: "T" });
    const wrapper = await mountCloud({ ...rows, connections: [{ id: 'c1', name: 'Primary', endpoint: 'https://s3.cn-east-1.qiniucs.com', bucket: 'alpha', buckets: ['alpha', 'beta'], region: 'cn-east-1' }] });
    await wrapper.find('.connection-edit').trigger('click'); await flushPromises();
    expect(wrapper.find('.connection-access-key input').element.value).toBe('AK');
    expect(wrapper.find('.connection-secret-key input').element.value).toBe('SK');
    expect(wrapper.find('.connection-bucket').text()).toContain('alpha');
    expect(wrapper.find('.connection-bucket').text()).toContain('beta');
    await wrapper.find('.connection-save').trigger('click'); await flushPromises();
    expect(cloud.saveConnection).toHaveBeenCalledWith(expect.objectContaining({ id: 'c1', bucket: 'alpha', buckets: ['alpha', 'beta'], accessKey: 'AK', secretKey: 'SK', sessionToken: 'T' }));
    wrapper.unmount();
  });
  it("clears stale URL inference and requires a probed bucket for a new address", async () => {
    const wrapper = await mountCloud(); await wrapper.find('.connection-add').trigger('click');
    await wrapper.find('.connection-url input').setValue('https://peerdav.s3.cn-east-1.qiniucs.com');
    await wrapper.find('.connection-url input').setValue('https://custom.example'); await wrapper.find('.connection-save').trigger('click');
    expect(cloud.saveConnection).not.toHaveBeenCalled(); expect(app.setError).toHaveBeenCalledWith('drive.bucketRequired');
    expect(wrapper.find('.connection-bucket').exists()).toBe(false); wrapper.unmount();
  });
  it("renders local-only removal confirmation with a red counted sync warning and cancellation is inert", async () => {
    confirmAction.mockResolvedValue(false);
    const wrapper = await mountCloud({ ...rows, mappings: [{ connectionId: 'c1' }, { connectionId: 'c1' }, { connectionId: 'other' }] });
    await wrapper.find('.connection-remove').trigger('click'); await flushPromises();
    const [vnode, , options] = confirmAction.mock.calls[0];
    const dialog = mount(defineComponent({ render: () => vnode }));
    expect(dialog.text()).toContain('不会删除远端文件'); expect(dialog.find('[data-test="sync-removal-warning"]').text()).toContain('2');
    expect(dialog.find('[data-test="sync-removal-warning"]').attributes('style')).toContain('color:'); expect(options.type).toBe('error');
    expect(cloud.deleteConnection).not.toHaveBeenCalled(); expect(cloud.deleteMapping).not.toHaveBeenCalled(); dialog.unmount(); wrapper.unmount();
  });
  it("removes only the local connection after explicit acceptance", async () => {
    const wrapper = await mountCloud({ ...rows, mappings: [] }); await wrapper.find('.connection-remove').trigger('click'); await flushPromises();
    expect(cloud.deleteConnection).toHaveBeenCalledWith('c1'); expect(cloud.deleteMapping).not.toHaveBeenCalled();
    expect(confirmAction.mock.calls[0][2].type).toBe('warning'); wrapper.unmount();
  });
  it("emits a catalog refresh event only after successful save or removal", async () => {
    const changed = vi.fn(); window.addEventListener('peerdav:cloud-changed', changed);
    const wrapper = await mountCloud();
    await wrapper.find('.connection-add').trigger('click');
    await wrapper.find('.connection-url input').setValue('https://peerdav.s3.cn-east-1.qiniucs.com');
    cloud.probeBuckets.mockResolvedValue([{ name: 'peerdav' }]);
    await wrapper.find('.connection-access-key input').setValue('AK'); await wrapper.find('.connection-secret-key input').setValue('SK');
    await wrapper.find('.connection-probe-run').trigger('click'); await flushPromises();
    cloud.saveConnection.mockRejectedValueOnce({ code: 'CLOUD_STORAGE', message: 'original' });
    await wrapper.find('.connection-save').trigger('click'); await flushPromises(); expect(changed).not.toHaveBeenCalled();
    await wrapper.find('.connection-save').trigger('click'); await flushPromises(); expect(changed).toHaveBeenCalledTimes(1);
    cloud.list.mockResolvedValue({ ...rows, connections: [] });
    await wrapper.find('.connection-remove').trigger('click'); await flushPromises(); expect(changed).toHaveBeenCalledTimes(2);
    expect(wrapper.find('.cloud-browse-entry').exists()).toBe(false);
    window.removeEventListener('peerdav:cloud-changed', changed); wrapper.unmount();
  });
  it("renders both locales", async () => {
    const wrapper = await mountCloud(); expect(wrapper.find('.connection-add').attributes('aria-label')).toBe('添加网盘'); setLocale('en'); await nextTick(); expect(wrapper.find('.connection-add').attributes('aria-label')).toBe('Add cloud drive'); wrapper.unmount();
  });
});
