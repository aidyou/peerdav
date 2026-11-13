import { describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { flushPromises, mount } from "@vue/test-utils";
const { listen } = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("element-plus/es/components/dialog/index", () => ({
  ElDialog: defineComponent({ props: ["modelValue"], setup(props, { slots }) { return () => props.modelValue ? h("section", [slots.default?.(), slots.footer?.()]) : null; } }),
}));
vi.mock("element-plus/es/components/button/index", async (importOriginal) => ({
  ...await importOriginal(),
  ElButton: defineComponent({ setup(_, { slots, attrs }) { return () => h("button", attrs, slots.default?.()); } }),
}));
import CapabilityProgressDialog from "../components/CapabilityProgressDialog.vue";
import OperationErrorDialog from "../components/OperationErrorDialog.vue";
import { setLocale } from "../lib/i18n";
const dialog = defineComponent({ props: ["modelValue"], setup(props, { slots }) { return () => props.modelValue ? h("section", [slots.default?.(), slots.footer?.()]) : null; } });
const button = defineComponent({ setup(_, { slots, attrs }) { return () => h("button", attrs, slots.default?.()); } });
const global = { stubs: { ElDialog: dialog, ElButton: button, "el-dialog": dialog, "el-button": button, teleport: true } };
describe("capability progress and readable errors", () => {
  it("shows real stages, dismisses to background and removes terminal probes", async () => {
    setLocale("zh-CN");
    let handler; const stop = vi.fn();
    listen.mockImplementation(async (_, callback) => { handler = callback; return stop; });
    const wrapper = mount(CapabilityProgressDialog, { global }); await flushPromises();
    expect(wrapper.text()).toBe("");
    const event = { scope: "scope", connectionName: "MinIO", capability: "move", stage: "checking" };
    handler({ payload: event }); await flushPromises();
    expect(wrapper.html(), document.body.innerHTML).toContain("安全移动检测");
    expect(wrapper.find("progress").attributes("value")).toBeUndefined();
    handler({ payload: { ...event, stage: "cleaning" } }); await flushPromises();
    expect(wrapper.text()).toContain("正在清理临时测试文件");
    await wrapper.find("button").trigger("click"); expect(wrapper.text()).toBe("");
    handler({ payload: { ...event, stage: "completed" } }); await flushPromises();
    handler({ payload: event }); await flushPromises(); expect(wrapper.text()).toContain("MinIO");
    handler({ payload: { ...event, stage: "failed" } }); await flushPromises(); expect(wrapper.text()).toBe("");
    wrapper.unmount(); expect(stop).toHaveBeenCalledOnce();
  });
  it("never renders diagnostic codes or details to users", () => {
    setLocale("zh-CN");
    const wrapper = mount(OperationErrorDialog, { global, props: { error: { code: "CLOUD_LOCAL_MAPPING_CONFLICT", message: "nested: technical", detail: "secret diagnostic", context: { drive: "GoogleDrive", userOperationKey: "errorOperation.createSyncDrive" } } } });
    expect(wrapper.text()).toContain("创建同步盘失败：所选本地文件夹已被 GoogleDrive");
    expect(wrapper.text()).not.toContain("CLOUD_LOCAL_MAPPING_CONFLICT");
    expect(wrapper.text()).not.toContain("secret diagnostic");
  });
});
