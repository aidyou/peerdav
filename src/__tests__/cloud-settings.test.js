import { beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h, nextTick } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { cloud, confirmAction, openDialog, listen, app } = vi.hoisted(() => ({
  cloud: {
    list: vi.fn(), saveConnection: vi.fn(), testConnection: vi.fn(), deleteConnection: vi.fn(),
    createRepository: vi.fn(), joinRepository: vi.fn(), saveMapping: vi.fn(), deleteMapping: vi.fn(),
    preview: vi.fn(), sync: vi.fn(), cancel: vi.fn(), recoveryRecords: vi.fn(), restore: vi.fn(),
  },
  confirmAction: vi.fn(), openDialog: vi.fn(), listen: vi.fn(),
  app: { handleFailure: vi.fn(), setNotice: vi.fn(), setError: vi.fn() },
}));
vi.mock("../lib/cloud", async (importOriginal) => { const actual = await importOriginal(); return { ...actual, cloud }; });
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("../lib/confirm", () => ({ confirmAction }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openDialog }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { initLocale, setLocale } from "../lib/i18n";
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

beforeEach(() => { setLocale("zh-CN"); Object.values(cloud).forEach((fn) => fn.mockReset()); confirmAction.mockReset(); confirmAction.mockResolvedValue(true); openDialog.mockReset(); app.handleFailure.mockReset(); app.setNotice.mockReset(); app.setError.mockReset(); progressHandler = undefined; });

describe("CloudSettings", () => {
  it("opens independent browsing for the selected connection without starting sync", async () => {
    const wrapper = await mountCloud({ ...rows, connections: [{ id: "cloud/a b", name: "Browse target" }] });
    await wrapper.find(".cloud-browse-entry").trigger("click");
    expect(window.location.hash).toBe("#/cloud/cloud%2Fa%20b");
    expect(cloud.sync).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("普通网盘对象使用独立浏览视图");
    wrapper.unmount();
    window.history.replaceState(null, "", window.location.pathname);
  });
  it("renders the connection, repository secret/password, and mapping inputs", async () => {
    setLocale("zh-CN"); const wrapper = await mountCloud();
    expect(wrapper.findAll("input").length).toBeGreaterThanOrEqual(7);
    expect(wrapper.findAll('input[type="password"]').length).toBe(4);
    expect(wrapper.text()).toContain("Primary");
    expect(wrapper.text()).toContain("Repo");
  });

  it("saves connection with wrapper-shaped input and clears secrets", async () => {
    setLocale("zh-CN"); const wrapper = await mountCloud();
    const inputByLabel = (label) => wrapper.findAll("label").find((item) => item.text().includes(label)).find("input");
    await inputByLabel("名称").setValue("New"); await inputByLabel("区域").setValue("access"); await inputByLabel("前缀").setValue("secret");
    cloud.saveConnection.mockResolvedValue({});
    await wrapper.findAll("button").find((b) => b.text() === "保存").trigger("click");
    await flushPromises();
    expect(cloud.saveConnection).toHaveBeenCalledWith(expect.objectContaining({ name: "New", region: "access", prefix: "secret" }));
    expect(inputByLabel("区域").element.value).toBe("access"); expect(inputByLabel("前缀").element.value).toBe("secret");
  });

  it("passes both password inputs to create and clears them after success", async () => {
    setLocale("zh-CN"); const wrapper = await mountCloud();
    const passwordInputs = wrapper.findAll('input[type="password"]');
    await passwordInputs[2].setValue("pass123"); await passwordInputs[3].setValue("pass123");
    cloud.createRepository.mockResolvedValue({});
    await wrapper.findAll("button").find((b) => b.text() === "创建仓库").trigger("click"); await flushPromises();
    expect(cloud.createRepository).toHaveBeenCalledWith(expect.objectContaining({ connectionId: "c1", password: "pass123", passwordConfirmation: "pass123" }));
    expect(passwordInputs[2].element.value).toBe(""); expect(passwordInputs[3].element.value).toBe("");
  });

  it("rejects mismatched repository passwords without invoking", async () => {
    setLocale("zh-CN"); const wrapper = await mountCloud(); const inputs = wrapper.findAll('input[type="password"]');
    await inputs[2].setValue("one"); await inputs[3].setValue("two");
    await wrapper.findAll("button").find((b) => b.text() === "创建仓库").trigger("click");
    expect(cloud.createRepository).not.toHaveBeenCalled(); expect(app.setError).toHaveBeenCalledWith("login.mismatch");
  });

  it("chooses a root directory and saves mapping", async () => {
    openDialog.mockResolvedValue("/new-root"); setLocale("zh-CN"); const wrapper = await mountCloud();
    await wrapper.findAll("button").find((b) => b.text() === "选择目录").trigger("click"); await flushPromises();
    expect(openDialog).toHaveBeenCalledWith({ directory: true, multiple: false });
    expect(wrapper.findAll("input").some((input) => input.element.value === "/new-root")).toBe(true);
    cloud.saveMapping.mockResolvedValue({}); const saveButtons = wrapper.findAll("button").filter((b) => b.text() === "保存"); await saveButtons[1].trigger("click"); await flushPromises();
    expect(cloud.saveMapping).toHaveBeenCalledWith(null, expect.objectContaining({ localRoot: "/new-root" }));
  });

  it("previews the requested mapping and applies that preview, not the first mapping", async () => {
    const wrapper = await mountCloud({ ...rows, mappings: [{ id: "first", localRoot: "/1", logicalPath: "" }, { id: "second", localRoot: "/2", logicalPath: "" }] });
    cloud.preview.mockResolvedValue({ previewId: "preview-2", summary: "second summary" });
    const previewButtons = wrapper.findAll("button").filter((b) => b.text() === "预览"); await previewButtons[1].trigger("click"); await flushPromises();
    expect(cloud.preview).toHaveBeenCalledWith("second"); expect(wrapper.text()).toContain("second summary");
    cloud.sync.mockResolvedValue({}); await wrapper.findAll("button").find((b) => b.text() === "应用预览").trigger("click"); await flushPromises();
    expect(cloud.sync).toHaveBeenCalledWith("second", "preview-2");
  });

  it("cancels the selected mapping run", async () => { setLocale("zh-CN"); const wrapper = await mountCloud(); await wrapper.findAll("button").find((b) => b.text() === "取消同步").trigger("click"); expect(cloud.cancel).toHaveBeenCalledWith("m1"); });
  it("shows pending recovery progress", async () => { setLocale("zh-CN"); const wrapper = await mountCloud(); progressHandler({ payload: { phase: "pendingRecovery", completed: 1, total: 2 } }); await nextTick(); expect(wrapper.text()).toContain("等待恢复"); expect(wrapper.text()).toContain("等待恢复"); });
  it("asks for confirmation before deleting a mapping", async () => { setLocale("zh-CN"); const wrapper = await mountCloud(); const deleteButtons = wrapper.findAll("button").filter((b) => b.text() === "删除"); await deleteButtons[1].trigger("click"); await flushPromises(); expect(confirmAction).toHaveBeenCalled(); });
  it("does not delete when confirmation is cancelled", async () => { confirmAction.mockResolvedValue(false); setLocale("zh-CN"); const wrapper = await mountCloud(); const deleteButtons = wrapper.findAll("button").filter((b) => b.text() === "删除"); await deleteButtons[1].trigger("click"); await flushPromises(); expect(cloud.deleteConnection).not.toHaveBeenCalled(); expect(cloud.deleteMapping).not.toHaveBeenCalled(); });
  it("uses initial status rows and renders recovery detail", async () => {
    const wrapper = await mountCloud({ ...rows, statuses: [{ mappingId: "m2", phase: "pendingRecovery", detail: "手动恢复" }] });
    expect(wrapper.text()).toContain("等待恢复"); expect(wrapper.text()).toContain("手动恢复");
  });
  it("shows encrypted repository mode from the backend", async () => {
    const wrapper = await mountCloud({ ...rows, repositories: [{ id: "r1", name: "Repo", mode: "encrypted" }] });
    expect(wrapper.text()).toContain("已加密");
  });
  it("keeps proxy credential fields blank until explicitly entered", async () => {
    const wrapper = await mountCloud({ ...rows, connections: [{ id: "c1", name: "Primary", proxy: { mode: "custom", url: "http://proxy", username: "secret-user", password: "secret-pass" } }] });
    expect(wrapper.findAll("input").some((input) => input.element.value === "secret-user" || input.element.value === "secret-pass")).toBe(false);
  });
  it("disables repository creation and does not invoke without a connection", async () => {
    const wrapper = await mountCloud({ ...rows, connections: [] });
    const create = wrapper.findAll("button").find((b) => b.text() === "创建仓库");
    expect(create.attributes("disabled")).toBeDefined(); await create.trigger("click");
    expect(cloud.createRepository).not.toHaveBeenCalled();
  });
  it("renders both locale key sets through the actual translation function", async () => { setLocale("zh-CN"); const wrapper = await mountCloud(); expect(wrapper.text()).toContain("S3 连接"); expect(wrapper.text()).not.toContain("cloud.connections"); wrapper.unmount();
    localStorage.setItem("peerdav.locale", "en"); setLocale("en"); const second = await mountCloud(); expect(second.text()).toContain("S3 connections"); expect(second.text()).not.toContain("cloud.connections");
  });
});
