import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { flushPromises, mount } from "@vue/test-utils";

const { nativeInvoke, confirmAction } = vi.hoisted(() => ({ nativeInvoke: vi.fn(), confirmAction: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: nativeInvoke }));
vi.mock("../lib/confirm", () => ({ confirmAction }));
import CloudBrowser from "../components/CloudBrowser.vue";
import { cloudObjects, objectChildren } from "../lib/cloud";
import { errorStrategy } from "../lib/invoke";
import { setLocale, t } from "../lib/i18n";
import en from "../locales/en";
import zh from "../locales/zh-CN";

const button = defineComponent({ setup(_, { slots, attrs }) { return () => h("button", attrs, slots.default?.()); } });
const input = defineComponent({ props: ["modelValue"], emits: ["update:modelValue"], setup(props, { attrs, emit }) { return () => h("input", { ...attrs, value: props.modelValue, onInput: (e) => emit("update:modelValue", e.target.value) }); } });
const select = defineComponent({ name: "CloudSelectStub", props: ["modelValue"], emits: ["update:modelValue"], setup(props, { slots, attrs, emit }) { return () => h("select", { ...attrs, value: props.modelValue, onChange: (e) => emit("update:modelValue", e.target.value) }, slots.default?.()); } });
const option = defineComponent({ props: ["value", "label"], setup(props) { return () => h("option", { value: props.value }, props.label); } });
const error = (code, message = "Original backend reason") => ({ code, message, detail: "private", context: null });
const page = (objects = [], next = null, complete = true) => ({ prefix: "", objects, next, complete, prefixProtected: false });
const file = { key: "a.txt", size: 7, etag: "etag-1", protected: false };
const deletePlan = (extra = {}) => ({ planId: "plan-1", expiresAtUnix: Date.now() / 1000 + 60, objectCount: 5, connectionRevision: 1, entries: Array.from({ length: 5 }, (_, i) => ({ key: `exact-${i}.txt`, size: i, etag: null })), ...extra });
const transfer = (phase = "running", extra = {}) => ({ transferId: "t1", connectionId: "c1", connectionRevision: 1, direction: "upload", phase, key: "a.txt", sizeHint: 7, error: null, recoveryRequired: false, ...extra });
const deferred = () => { let resolve; let reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
let handlers;
const wrappers = [];
async function browser() { const wrapper = mount(CloudBrowser, { props: { connectionId: "c1" }, global: { stubs: { "el-button": button, "el-input": input, "el-select": select, "el-option": option, "el-tag": { template: "<span><slot /></span>" } } } }); wrappers.push(wrapper); await flushPromises(); return wrapper; }
async function click(wrapper, selector) { await wrapper.find(selector).trigger("click"); await flushPromises(); }
async function prepare(wrapper) { await click(wrapper, '[data-test=prepare]'); }
function textButton(wrapper, label) { return wrapper.findAll("button").find((item) => item.text() === label); }
beforeEach(() => {
  vi.useFakeTimers(); nativeInvoke.mockReset(); confirmAction.mockReset(); confirmAction.mockResolvedValue(true); setLocale("en");
  handlers = { cloud_list: () => ({ connections: [{ id: "c1", name: "Main", prefix: "configured/" }, { id: "c2", name: "Other" }] }), cloud_object_list: () => page([file]), cloud_object_status: () => [], cloud_object_register_physical_identity: () => ({ physicalId: "physical" }), cloud_object_discover: () => ({ complete: true }), cloud_object_select_local: () => ({ token: "opaque", fileName: "a.txt", localPath: "/must-not-leak" }), cloud_object_upload: () => transfer(), cloud_object_download: () => transfer("running", { direction: "download" }), cloud_object_cancel: () => transfer("cancelling"), cloud_object_recovery_acknowledge: () => transfer("acknowledged"), cloud_object_delete_plan: () => deletePlan(), cloud_object_delete_execute: () => ({ deleted: [file.key], failed: [] }) };
  nativeInvoke.mockImplementation(async (command, args) => { if (!handlers[command]) throw new Error(`Unexpected command ${command}`); return handlers[command](args); });
});
afterEach(() => { wrappers.splice(0).forEach((wrapper) => { if (wrapper.exists()) wrapper.unmount(); }); vi.useRealTimers(); });

describe("CloudBrowser", () => {
  it("groups flat object keys as navigable virtual folders and keeps configured scope server-side", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([{ key: "docs/nested/a", size: 1 }]) : page([{ key: "docs/a", size: 1 }, file, { key: "docs/b", size: 2 }]);
    const wrapper = await browser(); expect(wrapper.findAll(".folder-link")).toHaveLength(1); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "", next: null });
    await click(wrapper, ".folder-link"); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
    await click(wrapper, '[data-test=up]'); expect(wrapper.find("code").text()).toBe("/");
    expect(objectChildren([{ key: "other/a" }], "docs/")).toEqual([]);
  });
  it("shows incomplete pagination until the final page and merges duplicate keys", async () => {
    handlers.cloud_object_list = ({ next }) => next ? page([file, { key: "b.txt", size: 3 }]) : page([file], "cursor", false);
    const wrapper = await browser(); expect(wrapper.find('[data-test=incomplete]').exists()).toBe(true);
    await click(wrapper, '[data-test=more]'); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "", next: "cursor" }); expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false); expect(wrapper.findAll(".cloud-tree")[0].findAll("li")).toHaveLength(2);
  });
  it("does not claim an empty or complete listing after loading failure", async () => { handlers.cloud_object_list = () => { throw error("CLOUD_TRANSPORT"); }; const wrapper = await browser(); expect(wrapper.text()).toContain("Original backend reason"); expect(wrapper.text()).not.toContain(t("cloudBrowser.empty")); expect(wrapper.find('[data-test=incomplete]').exists()).toBe(true); });
  it("shows true empty state only after complete listing", async () => { handlers.cloud_object_list = () => page(); const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.empty")); });
  it("defaults to unknown read-only and requires explicit identity plus complete discovery", async () => {
    handlers.cloud_object_discover = () => ({ complete: false }); const wrapper = await browser(); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined(); expect(wrapper.text()).toContain(t("cloudBrowser.unknownReadonly"));
    await prepare(wrapper); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_register_physical_identity", { connectionId: "c1", association: { kind: "new" } }); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined();
    handlers.cloud_object_discover = () => ({ complete: true }); await prepare(wrapper); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeUndefined(); expect(wrapper.text()).toContain(t("cloudBrowser.attemptOnly"));
  });
  it("supports explicit existing physical identity association", async () => { const wrapper = await browser(); wrapper.findAllComponents({ name: "ElSelect" })[1].vm.$emit("update:modelValue", "existing"); await flushPromises(); await wrapper.find('input[aria-label="Existing physical identity ID"]').setValue("physical-existing"); await prepare(wrapper); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_register_physical_identity", { connectionId: "c1", association: { kind: "existing", physicalId: "physical-existing" } }); });
  it("honors protected error codes and displays original message without matching text", async () => {
    handlers.cloud_object_upload = () => { throw error("CLOUD_OBJECT_PROTECTED", "anything at all"); }; const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=upload]'); expect(wrapper.text()).toContain("anything at all"); expect(wrapper.text()).toContain(t("cloudBrowser.protected")); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined(); expect(errorStrategy(error("CLOUD_DELETE_PLAN_STALE"))).toBe("retry");
  });
  it("confirms same-key ETag overwrite and sends only opaque local token", async () => { const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=upload]'); expect(confirmAction).toHaveBeenCalledWith(expect.stringContaining("etag-1"), t("cloudBrowser.upload")); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_upload", { connectionId: "c1", localToken: "opaque", key: "a.txt", overwriteEtag: "etag-1", overwriteConfirmed: true }); });
  it("does not upload if ETag overwrite confirmation is declined", async () => { confirmAction.mockResolvedValue(false); const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=upload]'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_upload")).toBe(false); });
  it("downloads without overwrite then confirms and reselects a token after backend conflict", async () => {
    let selects = 0; handlers.cloud_object_select_local = () => ({ token: `token-${++selects}` }); let attempts = 0; handlers.cloud_object_download = () => { if (++attempts === 1) throw error("CLOUD_PRECONDITION"); return transfer("running", { direction: "download" }); };
    const wrapper = await browser(); await textButton(wrapper, "Download").trigger("click"); await flushPromises();
    const calls = nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_download"); expect(calls.map(([, args]) => [args.localToken, args.overwriteConfirmed])).toEqual([["token-1", false], ["token-2", true]]); expect(confirmAction).toHaveBeenCalledOnce();
  });
  it("supports asynchronous failed-download precondition retry without auto overwrite", async () => {
    handlers.cloud_object_download = () => transfer("running", { direction: "download" }); const wrapper = await browser(); await textButton(wrapper, "Download").trigger("click"); await flushPromises(); handlers.cloud_object_status = () => [transfer("failed", { direction: "download", error: error("CLOUD_PRECONDITION") })]; await vi.advanceTimersByTimeAsync(1500); await flushPromises(); expect(confirmAction).not.toHaveBeenCalled(); await textButton(wrapper, t("cloudBrowser.retryDownload")).trigger("click"); await flushPromises(); expect(confirmAction).toHaveBeenCalledOnce(); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_select_local")).toHaveLength(2); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_download")[1][1].overwriteConfirmed).toBe(true);
  });
  it("shows cancellation acknowledgement and unknown recovery without fake percentages", async () => {
    handlers.cloud_object_status = () => [transfer()]; const wrapper = await browser(); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_cancel", { transferId: "t1" }); expect(wrapper.text()).toContain(t("cloudBrowser.phase.cancelling")); handlers.cloud_object_status = () => [transfer("unknown", { recoveryRequired: true })]; await vi.advanceTimersByTimeAsync(1500); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.unknown")); expect(wrapper.text()).toContain(t("cloudBrowser.recovery")); expect(wrapper.html()).not.toContain("progressbar"); expect(wrapper.text()).not.toContain("%");
  });
  it("plans deletion before confirming exact target prefix count and irreversible warning", async () => {
    handlers.cloud_object_delete_execute = () => ({ deleted: ["a"], failed: [{ key: "b", error: error("CLOUD_STORAGE") }] }); const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=delete-prefix]'); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_delete_plan", { connectionId: "c1", prefix: "" }); expect(confirmAction.mock.calls[0][1]).toBe(t("cloudBrowser.delete")); const review = mount({ render: () => confirmAction.mock.calls[0][0] }); expect(review.text()).toContain('Target prefix: ""'); expect(review.text()).toContain("5"); expect(review.text()).toContain("cannot be undone"); expect(review.find('[data-test=delete-keys]').element.style.overflow).toBe("auto"); expect(review.findAll("li").map((item) => item.text())).toEqual(deletePlan().entries.map((item) => JSON.stringify(item.key))); review.unmount(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_delete_execute", { connectionId: "c1", planId: "plan-1", confirmed: true }); expect(wrapper.text()).toContain("Deleted: 1; failed: 1"); expect(wrapper.text()).toContain("Original backend reason");
  });
  it("does not execute cancelled or expired delete plans", async () => { const wrapper = await browser(); await prepare(wrapper); confirmAction.mockResolvedValue(false); await click(wrapper, '[data-test=delete-prefix]'); confirmAction.mockResolvedValue(true); handlers.cloud_object_delete_plan = () => deletePlan({ planId: "expired", expiresAtUnix: 1 }); await click(wrapper, '[data-test=delete-prefix]'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false); expect(wrapper.text()).toContain("CLOUD_DELETE_PLAN_STALE"); });
  it("ignores stale directory page results after navigation", async () => { const late = deferred(); let count = 0; handlers.cloud_object_list = ({ prefix }) => { if (prefix) return page([{ key: "docs/new", size: 1 }]); if (++count === 1) return page([{ key: "docs/old", size: 1 }], "next", false); return late.promise; }; const wrapper = await browser(); await wrapper.find('[data-test=more]').trigger("click"); await click(wrapper, ".folder-link"); late.resolve(page([{ key: "stale", size: 2 }])); await flushPromises(); expect(wrapper.text()).toContain("new"); expect(wrapper.text()).not.toContain("stale"); });
  it("ignores pending identity confirmation after connection change", async () => { const late = deferred(); handlers.cloud_object_register_physical_identity = () => late.promise; const wrapper = await browser(); await wrapper.find('[data-test=prepare]').trigger("click"); await wrapper.setProps({ connectionId: "c2" }); await flushPromises(); late.resolve({ physicalId: "late" }); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_discover")).toBe(false); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined(); });
  it("cleans polling on unmount and ignores in-flight poll completion", async () => { const late = deferred(); handlers.cloud_object_status = () => late.promise; const wrapper = await browser(); wrapper.unmount(); late.resolve([transfer()]); await flushPromises(); await vi.advanceTimersByTimeAsync(4500); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_status")).toHaveLength(1); expect(vi.getTimerCount()).toBe(0); });
  it("does not execute a deletion confirmed after directory switch", async () => {
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]); const answer = deferred(); confirmAction.mockImplementation(() => answer.promise); const wrapper = await browser(); await prepare(wrapper); await wrapper.find('[data-test=delete-prefix]').trigger("click"); await flushPromises(); await click(wrapper, ".folder-link"); answer.resolve(true); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false);
  });
  it("ignores stale catalog failures after changing connections", async () => {
    const late = deferred(); let count = 0; handlers.cloud_list = () => ++count === 1 ? late.promise : { connections: [{ id: "c2", name: "Other" }] }; const wrapper = await browser(); await wrapper.setProps({ connectionId: "c2" }); await flushPromises(); late.reject(error("CLOUD_TRANSPORT", "obsolete catalog error")); await flushPromises(); expect(wrapper.text()).not.toContain("obsolete catalog error");
  });
  it("does not let an earlier poll overwrite a cancellation response", async () => {
    handlers.cloud_object_status = () => [transfer()]; const wrapper = await browser(); const late = deferred(); handlers.cloud_object_status = () => late.promise; await vi.advanceTimersByTimeAsync(1500); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); late.resolve([transfer()]); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.cancelling")); expect(wrapper.text()).not.toContain(t("cloudBrowser.cancel"));
  });
  it("renders cancelled and failed terminal states without a cancellation action", async () => {
    handlers.cloud_object_status = () => [transfer("cancelled"), transfer("failed", { transferId: "t2", error: error("CLOUD_STORAGE") })]; const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.cancelled")); expect(wrapper.text()).toContain(t("cloudBrowser.phase.failed")); expect(wrapper.text()).toContain("Original backend reason"); expect(textButton(wrapper, t("cloudBrowser.cancel"))).toBeUndefined();
  });
  it("whitelists all object payloads and strips arbitrary paths and association fields", async () => {
    const args = { connectionId: "c1", localToken: "opaque", key: "a", fileName: "a", path: "/secret", localPath: "/secret", extra: true };
    await cloudObjects.upload(args); await cloudObjects.download(args); await cloudObjects.registerIdentity({ ...args, association: { kind: "existing", physicalId: "p", path: "/secret" } }); await cloudObjects.list(args); await cloudObjects.discover(args); await cloudObjects.selectLocal({ ...args, direction: "upload" }); await cloudObjects.deletePlan(args); await cloudObjects.deleteExecute({ ...args, planId: "p", confirmed: true }); await cloudObjects.recoveryAcknowledge({ ...args, transferId: "t1", connectionRevision: 1, confirmed: true });
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_recovery_acknowledge", { transferId: "t1", connectionId: "c1", connectionRevision: 1, confirmed: true });
    for (const [, payload] of nativeInvoke.mock.calls) { expect(JSON.stringify(payload)).not.toContain("/secret"); expect(payload).not.toHaveProperty("extra"); }
  });
  it.each([
    ["missing entries", { entries: undefined }], ["count mismatch", { objectCount: 6 }],
    ["empty key", { objectCount: 1, entries: [{ key: "", size: 1, etag: null }] }],
    ["null entry", { objectCount: 1, entries: [null] }],
    ["duplicate keys", { objectCount: 2, entries: [file, file] }],
    ["missing key", { objectCount: 1, entries: [{ size: 1, etag: null }] }],
    ["illegal key", { objectCount: 1, entries: [{ key: "../escape", size: 1, etag: null }] }],
  ])("blocks malformed delete plans: %s", async (_, extra) => {
    handlers.cloud_object_delete_plan = () => deletePlan(extra); const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=delete-prefix]');
    expect(confirmAction).not.toHaveBeenCalled(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false); expect(wrapper.text()).toContain(t("cloudBrowser.invalidPlan"));
  });
  it("renders every exact key in a long scrollable confirmation, preserving escaped characters", async () => {
    const entries = Array.from({ length: 130 }, (_, i) => ({ key: `folder/key-${i}\nquoted\"`, size: i, etag: null }));
    handlers.cloud_object_delete_plan = () => deletePlan({ entries, objectCount: entries.length }); confirmAction.mockResolvedValue(false);
    const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=delete-prefix]'); const review = mount({ render: () => confirmAction.mock.calls[0][0] });
    expect(review.text()).toContain("130"); expect(review.findAll("li").map((item) => item.text())).toEqual(entries.map((item) => JSON.stringify(item.key))); expect(review.find("ul").element.style.maxHeight).toBe("40vh"); review.unmount();
  });
  it("uses prefix protection for read-only controls while allowing raw download", async () => {
    handlers.cloud_object_list = () => ({ ...page([file]), prefixProtected: true }); const wrapper = await browser();
    expect(wrapper.find('[data-test=prepare]').attributes("disabled")).toBeDefined(); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined(); expect(wrapper.find('[data-test=delete-prefix]').attributes("disabled")).toBeDefined();
    await textButton(wrapper, t("cloudBrowser.download")).trigger("click"); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_download")).toBe(true);
  });
  it("blocks protected single objects, ancestor-prefix deletion and protected overwrite, not downloads", async () => {
    handlers.cloud_object_list = () => page([{ ...file, protected: true }, { key: "docs/private/a", protected: true, size: 1 }, { key: "docs/public", protected: false, size: 1 }]);
    const wrapper = await browser(); await prepare(wrapper); expect(wrapper.find('[data-test=delete-prefix]').attributes("disabled")).toBeDefined();
    expect(wrapper.findAll(".cloud-actions").every((item) => item.findAll("button").at(-1).attributes("disabled") !== undefined)).toBe(true);
    await click(wrapper, '[data-test=upload]'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_upload")).toBe(false);
    await wrapper.find('input[aria-label="Object name (default: selected filename)"]').setValue("a.txt"); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined();
    await textButton(wrapper, t("cloudBrowser.download")).trigger("click"); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_download")).toBe(true);
  });
  it.each(["en", "zh-CN"])("requires explicit acknowledgement and explains local-only risk in %s", async (locale) => {
    setLocale(locale); handlers.cloud_object_status = () => [transfer("unknown", { key: "uncertain/key", recoveryRequired: true })]; const wrapper = await browser();
    expect(wrapper.text()).toContain('"uncertain/key"'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_recovery_acknowledge")).toBe(false);
    confirmAction.mockResolvedValue(false); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); expect(confirmAction).toHaveBeenCalledWith(t("cloudBrowser.acknowledgeConfirm", { key: '"uncertain/key"' }), t("cloudBrowser.acknowledge"));
    expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_recovery_acknowledge")).toBe(false);
    confirmAction.mockResolvedValue(true); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_recovery_acknowledge", { transferId: "t1", connectionId: "c1", connectionRevision: 1, confirmed: true });
    expect(wrapper.text()).toContain(t("cloudBrowser.acknowledgedNotice")); expect(textButton(wrapper, t("cloudBrowser.acknowledge"))).toBeUndefined(); expect(wrapper.text()).not.toContain(t("cloudBrowser.phase.completed"));
  });
  it("does not infer a legacy null transfer target", async () => {
    handlers.cloud_object_status = () => [transfer("unknown", { key: null, sizeHint: null })]; const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.unknownTarget"));
    await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); expect(confirmAction.mock.calls[0][0]).toContain(t("cloudBrowser.unknownTarget")); expect(confirmAction.mock.calls[0][0]).not.toContain("a.txt");
  });
  it("prevents a stale poll from overwriting acknowledgement and ignores confirmation after route change", async () => {
    handlers.cloud_object_status = () => [transfer("unknown")]; const wrapper = await browser(); const late = deferred(); handlers.cloud_object_status = () => late.promise;
    await vi.advanceTimersByTimeAsync(1500); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); late.resolve([transfer("unknown")]); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.acknowledged"));
    handlers.cloud_object_status = () => [transfer("unknown")]; await vi.advanceTimersByTimeAsync(1500); const answer = deferred(); confirmAction.mockImplementation(() => answer.promise); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); await wrapper.setProps({ connectionId: "c2" }); await flushPromises(); answer.resolve(true); await flushPromises(); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_recovery_acknowledge")).toHaveLength(1);
  });
  it("shows stop timeout guidance without hiding the original error", async () => {
    handlers.cloud_object_status = () => [transfer()]; handlers.cloud_object_cancel = () => { throw error("CLOUD_OBJECT_STOP_TIMEOUT"); }; const wrapper = await browser(); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); expect(wrapper.text()).toContain("Original backend reason"); expect(wrapper.text()).toContain(t("cloudBrowser.stopTimeout")); expect(errorStrategy(error("CLOUD_OBJECT_STOP_TIMEOUT"))).toBe("show");
  });
  it("has identical real bilingual keys and renders both locales without raw keys", async () => {
    const keys = Object.keys(en).filter((key) => key.startsWith("cloudBrowser.")); expect(keys.sort()).toEqual(Object.keys(zh).filter((key) => key.startsWith("cloudBrowser.")).sort()); for (const language of ["en", "zh-CN"]) { setLocale(language); const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.title")); expect(wrapper.text()).not.toContain("cloudBrowser."); for (const key of keys) expect(t(key)).not.toBe(key); wrapper.unmount(); }
  });
});
