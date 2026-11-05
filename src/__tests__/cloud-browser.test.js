import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defineComponent, h } from "vue";
import { DOMWrapper, flushPromises, mount } from "@vue/test-utils";

const { nativeInvoke, confirmAction, app } = vi.hoisted(() => ({ nativeInvoke: vi.fn(), confirmAction: vi.fn(), app: { handleFailure: vi.fn(), setNotice: vi.fn(), setInfo: vi.fn(), setError: vi.fn() } }));
vi.mock("../lib/appContext", () => ({ useAppContext: () => app }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: nativeInvoke }));
vi.mock("../lib/confirm", () => ({ confirmAction }));
vi.mock("../components/CloudSyncDrives.vue", () => ({ default: { props: ['connectionId', 'focusId'], emits: ['changed'], template: '<div data-test="sync-manager">{{ connectionId }}<button data-test="catalog-changed" @click="$emit(\'changed\')">Changed</button></div>' } }));
import cloudBrowserSource from "../components/CloudBrowser.vue?raw";
import downloadDialogSource from "../components/TransferQueueDialog.vue?raw";
import uploadDialogSource from "../components/UploadBatchDialog.vue?raw";
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
const dialog = defineComponent({ props: ["modelValue", "title"], setup(props, { slots, attrs }) { return () => props.modelValue ? h("div", { ...attrs, "data-test": "dialog" }, [props.title ? h("h3", props.title) : null, slots.default?.(), slots.footer?.()]) : null; } });
const progress = defineComponent({ props: ["percentage"], setup(props) { return () => h("div", { "data-test": "progress", "data-percentage": String(props.percentage) }); } });
const error = (code, message = "Original backend reason") => ({ code, message, detail: "private", context: null });
const page = (objects = [], next = null, complete = true) => ({ prefix: "", objects, next, complete, prefixProtected: false });
const file = { key: "a.txt", size: 7, etag: "etag-1", protected: false };
const deletePlan = (extra = {}) => ({ planId: "plan-1", expiresAtUnix: Date.now() / 1000 + 60, objectCount: 5, connectionRevision: 1, entries: Array.from({ length: 5 }, (_, i) => ({ key: `exact-${i}.txt`, size: i, etag: null })), ...extra });
const transfer = (phase = "running", extra = {}) => ({ transferId: "t1", connectionId: "c1", connectionRevision: 1, direction: "upload", phase, key: "a.txt", sizeHint: 7, error: null, recoveryRequired: false, ...extra });
const deferred = () => { let resolve; let reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const queueSummary = (phase = 'running', extra = {}) => ({ queueId: 'queue-1', backend: { backend: 's3', connectionId: 'c1' }, phase, totalFiles: 1, completedFiles: 0, skippedFiles: 0, conflict: null, error: null, ...extra });
const uploadSummary = (phase = 'running', extra = {}) => ({ batchId: 'batch', queueId: 'batch', phase, completedFiles: 0, completedBytes: 0, skippedFiles: 0, conflict: null, ...extra });
let handlers;
const wrappers = [];
// Conflict dialogs intentionally teleport outside the queue/browser root.
function findInDocument(wrapper, selector) {
  const local = wrapper.find(selector);
  if (local.exists()) return local;
  const node = document.querySelector(selector);
  return node ? new DOMWrapper(node) : local;
}
async function browser(mode = "list") { if (mode !== null) localStorage.setItem("peerdav.cloudView", mode); const wrapper = mount(CloudBrowser, { attachTo: document.body, props: { connectionId: "c1" }, global: { stubs: { "el-button": button, "el-input": input, "el-select": select, "el-option": option, "el-dialog": dialog, "el-progress": progress, "el-tag": { template: "<span><slot /></span>" } } } }); wrappers.push(wrapper); await flushPromises(); return wrapper; }
async function click(wrapper, selector) { await findInDocument(wrapper, selector).trigger("click"); await flushPromises(); }
async function prepare() { await flushPromises(); }
function textButton(wrapper, label) { return wrapper.findAll("button").find((item) => item.text() === label); }
beforeEach(() => {
  vi.useFakeTimers(); nativeInvoke.mockReset(); confirmAction.mockReset(); confirmAction.mockResolvedValue(true); setLocale("en");
  localStorage.removeItem("peerdav.cloudView");
  handlers = { cloud_list: () => ({ connections: [{ id: "c1", name: "Main", bucket: "main", buckets: ["main", "photos"], prefix: "configured/" }, { id: "c2", name: "Other", bucket: "main", buckets: ["main"] }] }), cloud_object_select_bucket: () => ({}), cloud_object_list: () => page([file]), cloud_object_status: () => [], cloud_object_discover: () => ({ complete: true }), cloud_object_prepare: () => ({ physicalId: "physical", discovered: true }), cloud_object_select_local: () => ({ token: "opaque", fileName: "a.txt", localPath: "/must-not-leak" }), cloud_object_upload: () => transfer(), cloud_object_download: () => transfer("running", { direction: "download" }), cloud_object_cancel: () => transfer("cancelling"), reveal_local_directory: () => ({}), cloud_object_recovery_acknowledge: () => transfer("acknowledged"), cloud_object_delete_plan: () => deletePlan(), cloud_object_delete_execute: () => ({ deleted: [file.key], failed: [] }) };
  Object.assign(handlers, {
    transfer_select_download: () => queueSummary('selected'), transfer_start_download: () => queueSummary(),
    transfer_status_download: () => queueSummary(), transfer_page_download: () => ({ items: [{ remotePath: 'a.txt', relativePath: 'a.txt', phase: 'running' }], nextOffset: null }),
    transfer_resolve_download: () => null, transfer_cancel_download: () => { handlers.transfer_status_download = () => queueSummary('canceled'); },
    transfer_release_download: () => null, transfer_reveal_download: () => null,
    cloud_object_select_upload_directory: () => uploadSummary('selected'), cloud_object_start_upload_directory: () => uploadSummary(),
    cloud_object_status_upload_directory: () => uploadSummary(), cloud_object_page_upload_directory: () => ({ entries: [] }),
    cloud_object_cancel_upload_directory: () => { handlers.cloud_object_status_upload_directory = () => uploadSummary('cancelled'); },
    cloud_object_release_upload_directory: () => null, cloud_object_resolve_upload_directory_conflict: () => null,
  });
  nativeInvoke.mockImplementation(async (command, args) => { if (!handlers[command]) throw new Error(`Unexpected command ${command}`); return handlers[command](args); });
});
afterEach(() => { wrappers.splice(0).forEach((wrapper) => { if (wrapper.exists()) wrapper.unmount(); }); document.body.innerHTML = ''; vi.useRealTimers(); });

describe("selection-bar action reset", () => {
  it.each(['list', 'gallery'].flatMap((view) => ['single', 'folder', 'files'].map((mode) => [view, mode])))('captures %s %s downloads before clearing, including picker cancellation', async (view, mode) => {
    handlers.cloud_object_list = () => page(mode === 'folder' ? [{ key: 'docs/a.txt' }] : [file, { key: 'b.txt', size: 2 }]);
    const pending = deferred();
    handlers.transfer_select_download = () => pending.promise;
    const wrapper = await browser(view);
    const entrySelector = view === 'gallery' ? '.gallery-thumb' : 'tbody tr';
    await wrapper.findAll(entrySelector)[0].trigger('click');
    if (mode === 'files') await wrapper.findAll(entrySelector)[1].trigger('click', { ctrlKey: true });
    await flushPromises();
    await wrapper.find('.selection-actions [data-test=download]').trigger('click');
    expect(wrapper.find('.selection-bar').exists()).toBe(false);
    expect(wrapper.findAll(`${entrySelector}.is-selected`)).toHaveLength(0);
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_select_download', {
      backend: { backend: 's3', connectionId: 'c1' },
      selection: mode === 'files' ? { mode, paths: ['a.txt', 'b.txt'] } : { mode, path: mode === 'folder' ? 'docs/' : 'a.txt' },
    });
    pending.resolve(null); await flushPromises();
    expect(wrapper.find('.selection-bar').exists()).toBe(false);
  });
  it('clears before delete planning and leaves cancellation unselected', async () => {
    const pending = deferred();
    handlers.cloud_object_delete_plan = () => pending.promise;
    confirmAction.mockResolvedValue(false);
    const wrapper = await browser();
    await wrapper.find('[data-test=file-row]').trigger('click');
    await wrapper.find('[data-test=delete-file]').trigger('click');
    expect(wrapper.find('.selection-actions').exists()).toBe(false);
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_delete_plan', { connectionId: 'c1', prefix: 'a.txt' });
    pending.resolve(deletePlan({ objectCount: 1, entries: [{ key: 'a.txt', size: 7, etag: null }] }));
    await flushPromises();
    expect(confirmAction).toHaveBeenCalled();
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'cloud_object_delete_execute')).toBe(false);
    expect(wrapper.find('.selection-actions').exists()).toBe(false);
  });
  it('clears folder selection before opening with the captured key', async () => {
    handlers.cloud_object_list = () => page([{ key: 'docs/a.txt' }]);
    const wrapper = await browser();
    await wrapper.find('[data-test=folder-row]').trigger('click');
    const pending = deferred(); handlers.cloud_object_list = () => pending.promise;
    await textButton(wrapper, t('peer.open')).trigger('click');
    expect(wrapper.find('.selection-actions').exists()).toBe(false);
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_list', { connectionId: 'c1', prefix: 'docs/', next: null });
    pending.resolve(page([])); await flushPromises();
  });
});

describe("streaming directory upload", () => {
  const calls = (command) => nativeInvoke.mock.calls.filter(([name]) => name === `cloud_object_${command}_upload_directory`);
  const summary = (phase = 'running', extra = {}) => ({ batchId: 'batch', connectionId: 'c1', connectionRevision: 1, bucket: 'main', prefix: '', rootName: 'tree', phase, currentRelativePath: null, completedFiles: 0, completedBytes: 0, skippedFiles: 0, skippedSymlinks: 1, skippedEmptyDirectories: 2, conflict: null, error: null, recoveryRequired: false, ...extra });
  const entry = (name, kind = 'file') => ({ name, relativePath: `tree/${name}`, kind, size: 7, directoryToken: kind === 'directory' ? 'child' : null, phase: 'unscanned' });
  const directPage = (entries = [entry('a.txt'), entry('deep', 'directory')], extra = {}) => ({ directoryToken: 'root', parentToken: null, relativePath: 'tree', entries, nextCursor: null, ...extra });
  beforeEach(() => {
    app.handleFailure.mockClear(); app.setNotice.mockClear(); app.setInfo.mockClear(); app.setError.mockClear();
    handlers.cloud_object_select_upload_directory = () => summary('selected');
    handlers.cloud_object_start_upload_directory = () => summary();
    handlers.cloud_object_status_upload_directory = () => summary();
    handlers.cloud_object_page_upload_directory = () => directPage();
    handlers.cloud_object_cancel_upload_directory = () => { handlers.cloud_object_status_upload_directory = () => summary('cancelled'); };
    handlers.cloud_object_release_upload_directory = () => null;
    handlers.cloud_object_resolve_upload_directory_conflict = () => null;
  });
  it('shows the fixed upload directory and its direct children without navigation controls', async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    expect(calls('start')[0][1]).toEqual({ connectionId: 'c1', batchId: 'batch', conflictPolicy: 'ask' });
    expect(nativeInvoke.mock.calls.findIndex(([name]) => name === 'cloud_object_page_upload_directory')).toBeLessThan(nativeInvoke.mock.calls.findIndex(([name]) => name === 'cloud_object_start_upload_directory'));
    const dialog = wrapper.find('[data-test=upload-dialog]');
    expect(dialog.text()).toContain('tree');
    expect(dialog.text()).toContain('a.txt');
    expect(dialog.text()).toContain('deep');
    for (const test of ['upload-up', 'upload-page-reset', 'upload-next', 'upload-enter']) {
      expect(wrapper.find(`[data-test=${test}]`).exists()).toBe(false);
    }
    expect(calls('page')).toHaveLength(1);
  });
  it('shows more than 100 completed files without percentage or accumulated rows', async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    handlers.cloud_object_status_upload_directory = () => summary('completed', { completedFiles: 10001, completedBytes: 70007, totalFiles: 10001 });
    await vi.advanceTimersByTimeAsync(4500); await flushPromises();
    expect(wrapper.find('.dialog-footer').text()).toContain('10001/10001');
    expect(wrapper.find('[data-test=upload-dialog] [data-test=progress]').exists()).toBe(false);
    expect(wrapper.find('[data-test=upload-dialog]').text()).not.toContain('%');
    expect(wrapper.findAll('[data-test=upload-dialog] li')).toHaveLength(2);
    expect(wrapper.findAll('[data-test=upload-dialog] li').every((row) => row.attributes('data-status') === 'unscanned')).toBe(true);
    expect(app.setNotice).toHaveBeenCalledTimes(1);
    expect(calls('release')).toHaveLength(0);
    await click(wrapper, '[data-test=close-upload]'); expect(calls('release')).toHaveLength(1);
  });
  it.each(['skip', 'overwrite', 'cancel'])('requires explicit %s conflict choice and forwards the checkbox', async (decision) => {
    handlers.cloud_object_start_upload_directory = () => summary('conflict', { conflict: { conflictId: 'conflict-a', relativePath: 'tree/a.txt', key: 'target/tree/a.txt', etag: 'etag' } });
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    expect(findInDocument(wrapper, '[data-test=upload-conflict]').text()).toContain('target/tree/a.txt');
    expect(confirmAction).not.toHaveBeenCalled();
    await findInDocument(wrapper, '[data-test=upload-apply-remaining]').setValue(true);
    await click(wrapper, `[data-test=${decision === 'cancel' ? 'upload-conflict-cancel' : `upload-${decision}`}]`);
    if (decision === 'cancel') {
      expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_cancel_upload_directory', { connectionId: 'c1', batchId: 'batch' });
      expect(calls('resolve')).toHaveLength(0);
    } else expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_resolve_upload_directory_conflict', { connectionId: 'c1', batchId: 'batch', conflictId: 'conflict-a', decision, applyToRemaining: true });
  });
  it.each(['success', 'expired'])('keeps a new same-path conflict intact after a late %s response', async (response) => {
    const conflictA = { conflictId: 'conflict-a', relativePath: 'tree/a.txt', key: 'tree/a.txt', etag: 'etag-a' };
    const conflictB = { ...conflictA, conflictId: 'conflict-b', etag: 'etag-b' };
    handlers.cloud_object_start_upload_directory = () => summary('conflict', { conflict: conflictA });
    handlers.cloud_object_status_upload_directory = handlers.cloud_object_start_upload_directory;
    const pending = deferred();
    handlers.cloud_object_resolve_upload_directory_conflict = () => pending.promise;
    const resolutions = () => nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_resolve_upload_directory_conflict');
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    await findInDocument(wrapper, '[data-test=upload-apply-remaining]').setValue(true);
    await vi.advanceTimersByTimeAsync(1500); await flushPromises();
    expect(findInDocument(wrapper, '[data-test=upload-apply-remaining]').element.checked).toBe(true);
    await click(wrapper, '[data-test=upload-skip]');
    expect(resolutions()).toHaveLength(1);
    expect(resolutions()[0][1]).toEqual({ connectionId: 'c1', batchId: 'batch', conflictId: 'conflict-a', decision: 'skip', applyToRemaining: true });
    handlers.cloud_object_status_upload_directory = () => summary('conflict', { conflict: conflictB });
    await vi.advanceTimersByTimeAsync(1500); await flushPromises();
    expect(findInDocument(wrapper, '[data-test=upload-conflict]').text()).toContain('etag-b');
    expect(findInDocument(wrapper, '[data-test=upload-apply-remaining]').element.checked).toBe(false);
    for (const choice of ['upload-skip', 'upload-overwrite', 'upload-conflict-cancel']) {
      expect(findInDocument(wrapper, `[data-test=${choice}]`).attributes('disabled')).toBeDefined();
      await click(wrapper, `[data-test=${choice}]`);
    }
    expect(resolutions()).toHaveLength(1);
    const statusCount = calls('status').length;
    if (response === 'expired') pending.reject(error('CLOUD_DIRECTORY_CONFLICT_EXPIRED'));
    else pending.resolve(null);
    await flushPromises();
    expect(calls('status')).toHaveLength(statusCount);
    expect(findInDocument(wrapper, '[data-test=upload-conflict]').text()).toContain('etag-b');
    expect(findInDocument(wrapper, '[data-test=upload-skip]').attributes('disabled')).toBeUndefined();
    expect(app.handleFailure).not.toHaveBeenCalled();
    handlers.cloud_object_resolve_upload_directory_conflict = () => null;
    await click(wrapper, '[data-test=upload-skip]');
    expect(resolutions()).toHaveLength(2);
    expect(resolutions()[1][1]).toEqual({ connectionId: 'c1', batchId: 'batch', conflictId: 'conflict-b', decision: 'skip', applyToRemaining: false });
  });
  it('disables overwrite without ETag', async () => {
    handlers.cloud_object_start_upload_directory = () => summary('conflict', { conflict: { conflictId: 'conflict-a', relativePath: 'tree/a', key: 'tree/a', etag: null } });
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    expect(findInDocument(wrapper, '[data-test=upload-overwrite]').attributes('disabled')).toBeDefined();
  });
  it('cancels and joins but retains terminal browsing until explicit close', async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    await click(wrapper, '[data-test=cancel-upload]');
    expect(calls('cancel')).toHaveLength(1); expect(calls('release')).toHaveLength(0);
    expect(wrapper.find('[data-test=close-upload]').exists()).toBe(true);
    expect(app.handleFailure).not.toHaveBeenCalled();
  });
  it.each(['connection', 'bucket', 'unmount'])('releases late picker and start responses on %s changes', async (change) => {
    for (const stage of ['select', 'start']) {
      const pending = deferred(); handlers[`cloud_object_${stage}_upload_directory`] = () => pending.promise;
      const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
      if (change === 'connection') await wrapper.setProps({ connectionId: 'c2' });
      else if (change === 'bucket') await wrapper.findAll('.bucket-link')[1].trigger('click');
      else wrapper.unmount();
      await flushPromises(); pending.resolve(summary(stage === 'select' ? 'selected' : 'running')); await flushPromises();
      expect(calls('release').at(-1)[1]).toEqual({ connectionId: 'c1', batchId: 'batch' });
      if (stage === 'start') expect(calls('cancel').length).toBeGreaterThan(0);
      handlers.cloud_object_select_upload_directory = () => summary('selected');
    }
  });
  it('ignores an old running poll after cancellation and never overlaps status requests', async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    const pending = deferred(); let count = 0;
    handlers.cloud_object_status_upload_directory = () => ++count === 1 ? pending.promise : summary('cancelled');
    handlers.cloud_object_cancel_upload_directory = () => null;
    await vi.advanceTimersByTimeAsync(1500);
    await wrapper.find('[data-test=cancel-upload]').trigger('click'); await flushPromises();
    expect(calls('status')).toHaveLength(1);
    pending.resolve(summary()); await flushPromises();
    expect(wrapper.find('[data-test=close-upload]').exists()).toBe(true);
    expect(wrapper.find('[data-test=cancel-upload]').exists()).toBe(false);
  });
  it.each(['connection', 'bucket', 'unmount'])('cancels and releases a running batch on %s change', async (change) => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    if (change === 'connection') await wrapper.setProps({ connectionId: 'c2' });
    else if (change === 'bucket') await wrapper.findAll('.bucket-link')[1].trigger('click');
    else wrapper.unmount();
    await flushPromises(); expect(calls('cancel')).toHaveLength(1); expect(calls('release')).toHaveLength(1);
  });
  it('keeps batch polling independent from remote navigation and suppresses overlapping polls', async () => {
    handlers.cloud_object_list = ({ prefix }) => page(prefix ? [] : [{ key: 'other/file.txt' }]);
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    await wrapper.find('[data-test=folder-row]').trigger('dblclick'); await flushPromises();
    const pending = deferred(); handlers.cloud_object_status_upload_directory = () => pending.promise;
    await vi.advanceTimersByTimeAsync(4500); expect(calls('status')).toHaveLength(1);
    pending.resolve(summary('completed', { completedFiles: 250, totalFiles: 250 })); await flushPromises();
    expect(wrapper.find('[data-test=upload-dialog] .dialog-footer').text()).toContain('250/250');
    expect(calls('start')).toHaveLength(1);
  });
  it('displays terminal failure and recovery without duplicating backend error events', async () => {
    handlers.cloud_object_start_upload_directory = () => summary('failed', { error: error('CLOUD_TRANSPORT', 'upload failed'), recoveryRequired: true });
    handlers.cloud_object_status_upload_directory = handlers.cloud_object_start_upload_directory;
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    expect(wrapper.find('[data-test=upload-dialog]').text()).toContain('upload failed');
    expect(wrapper.find('[data-test=upload-dialog]').text()).toContain(t('cloudBrowser.uploadRecovery'));
    expect(app.handleFailure).not.toHaveBeenCalled(); expect(calls('release')).toHaveLength(0);
  });
  it('reports real IPC failure but not cancellation', async () => {
    handlers.cloud_object_start_upload_directory = () => { throw error('CLOUD_TRANSPORT'); };
    const wrapper = await browser(); await click(wrapper, '[data-test=upload-directory]');
    expect(app.handleFailure).toHaveBeenCalledTimes(1);
    expect(wrapper.find('[data-test=retry-load]').exists()).toBe(false);
  });
  it('does not turn a canceled download transfer into a listing retry', async () => {
    handlers.transfer_start_download = () => queueSummary('canceled', { error: error('CLOUD_CANCELLED') });
    const wrapper = await browser(); await wrapper.find('tbody tr').trigger('click'); await click(wrapper, '[data-test=download]');
    expect(wrapper.find('.cloud-error').exists()).toBe(false); expect(app.handleFailure).not.toHaveBeenCalled();
  });
  it.each(['upload', 'upload-directory', 'download'])('silences canceled %s pickers', async (action) => {
    handlers.transfer_select_download = handlers.cloud_object_select_upload_directory = () => { throw error('CLOUD_CANCELLED'); };
    const wrapper = await browser(); if (action === 'download') await wrapper.find('tbody tr').trigger('click');
    await click(wrapper, `[data-test=${action}]`);
    expect(errorStrategy(error('CLOUD_CANCELLED'))).toBe('ignore');
    expect(wrapper.find('.cloud-error').exists()).toBe(false); expect(app.handleFailure).not.toHaveBeenCalled();
  });
});

describe("CloudBrowser", () => {
  it("defaults to icons, preserves stored modes and tolerates unavailable storage", async () => {
    expect((await browser(null)).find('.icon-grid').exists()).toBe(true);
    for (const mode of ['list', 'columns', 'gallery']) {
      expect((await browser(mode)).find(`[aria-label="${t(`cloudBrowser.${mode}`)}"]`).attributes('aria-pressed')).toBe('true');
    }
    localStorage.setItem('peerdav.cloudView', 'invalid');
    expect((await browser(null)).find('.icon-grid').exists()).toBe(true);
    const read = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('disabled'); });
    const write = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('disabled'); });
    try {
      const wrapper = await browser(null);
      expect(wrapper.find('.icon-grid').exists()).toBe(true);
      await click(wrapper, '[aria-label="List"]');
      expect(wrapper.find('.file-table').exists()).toBe(true);
    } finally { read.mockRestore(); write.mockRestore(); }
  });
  it("formats the fourth list column from modified only, including missing and invalid values", async () => {
    const seconds = 1700000000;
    handlers.cloud_object_list = () => page([{ ...file, modified: seconds }, { key: 'b.txt', size: 1, etag: '1700000000' }, { key: 'c.txt', modified: 'invalid' }]);
    const wrapper = await browser();
    const values = wrapper.findAll('tbody tr').map((row) => row.findAll('td')[3].text());
    expect(values).toEqual([new Intl.DateTimeFormat(document.documentElement.lang || undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(seconds * 1000)), '—', '—']);
  });
  it("falls back to created time, prefers modified and hides unusable time columns", async () => {
    let objects = [{ ...file, modified: 'invalid', created: 0 }, { key: 'b.txt', created: null }];
    handlers.cloud_object_list = () => page(objects);
    const wrapper = await browser();
    expect(wrapper.findAll('thead th')[3].text()).toBe(t('cloudBrowser.created'));
    expect(wrapper.findAll('tbody tr')[0].findAll('td')[3].text()).not.toBe('—');
    expect(wrapper.findAll('tbody tr')[1].findAll('td')[3].text()).toBe('—');
    objects = [{ ...file, modified: 1700000000, created: 1600000000 }];
    await click(wrapper, '[data-test=refresh]');
    expect(wrapper.findAll('thead th')[3].text()).toBe(t('peer.modified'));
    objects = [file, { key: 'b.txt', modified: null, created: ' ' }, { key: 'c.txt', modified: false, created: Infinity }, { key: 'd.txt', modified: 1e100, created: 'invalid' }];
    await click(wrapper, '[data-test=refresh]');
    expect(wrapper.findAll('thead th')).toHaveLength(3);
    expect(wrapper.findAll('tbody tr').every((row) => row.findAll('td').length === 3)).toBe(true);
  });
  it("normalizes reverse additive selections to listing order and clears stale selection on refresh", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }, { key: 'c.txt', size: 3 }]);
    handlers.cloud_object_download_batch = ({ items }) => ({ transfers: items.map((item, index) => transfer('running', { direction: 'download', key: item.key, transferId: `t${index}` })) });
    const wrapper = await browser();
    const rows = wrapper.findAll('tbody tr');
    await rows[2].trigger('click'); await rows[0].trigger('click', { ctrlKey: true });
    expect(wrapper.find('.selection-actions [data-test=download]').classes()).toContain('el-button--primary');
    expect(wrapper.find('.selection-actions [data-test=download]').classes()).toContain('is-plain');
    await click(wrapper, '.selection-actions [data-test=download]');
    expect(nativeInvoke.mock.calls.find(([name]) => name === 'transfer_select_download')[1].selection).toEqual({ mode: 'files', paths: ['a.txt', 'c.txt'] });
    await click(wrapper, '[data-test=refresh]');
    expect(wrapper.find('.selection-bar').exists()).toBe(false);
  });
  it("keeps ancestor columns while loading siblings and rejects obsolete navigation results", async () => {
    const first = deferred(); const second = deferred();
    handlers.cloud_object_list = ({ prefix }) => prefix === 'a/' ? first.promise : prefix === 'b/' ? second.promise : page([{ key: 'a/x' }, { key: 'b/x' }]);
    const wrapper = await browser('columns');
    await click(wrapper, '[data-key="a/"]');
    expect(wrapper.findAll('.column')).toHaveLength(2);
    expect(wrapper.find('.column-loading .browser-skeleton--columns').attributes('aria-label')).toBe(t('common.loading'));
    expect(wrapper.findAll('.column')[0].find('.browser-skeleton').exists()).toBe(false);
    expect(wrapper.findAll('.column')[0].text()).toContain('a');
    expect(wrapper.find('.file-view--placeholder').exists()).toBe(false);
    expect(wrapper.find('.column-empty').exists()).toBe(false);
    await click(wrapper, '[data-key="b/"]');
    second.resolve(page([{ key: 'b/new.txt', size: 2 }])); await flushPromises();
    expect(wrapper.find('.selection-summary').text()).toContain('b');
    first.resolve(page([{ key: 'a/obsolete.txt' }])); await flushPromises();
    expect(wrapper.find('.column-view').text()).toContain('new.txt');
    expect(wrapper.find('.column-view').text()).not.toContain('obsolete.txt');
    expect(nativeInvoke.mock.calls.filter(([name, args]) => name === 'cloud_object_list' && args.prefix === '')).toHaveLength(1);
    await click(wrapper, '[data-key="b/new.txt"]');
    expect(wrapper.find('.selection-summary').text()).toContain('new.txt');
  });
  it.each(['icons', 'columns', 'gallery'])("uses a %s skeleton for the first pending listing and removes it on failure", async (mode) => {
    const listing = deferred();
    handlers.cloud_object_list = () => listing.promise;
    const wrapper = await browser(mode);
    const skeleton = wrapper.find(`.browser-skeleton--${mode}`);
    expect(skeleton.exists()).toBe(true);
    expect(skeleton.find('[aria-hidden="true"]').exists()).toBe(true);
    expect(skeleton.text()).toBe('');
    if (mode === 'columns') expect(wrapper.findAll('.column')).toHaveLength(1);
    listing.reject(error('CLOUD_TRANSPORT'));
    await flushPromises();
    expect(wrapper.find(`.file-view .browser-skeleton--${mode}`).exists()).toBe(false);
    expect(wrapper.find('[role=alert]').text()).toContain('Original backend reason');
  });
  it("removes a failed right-column skeleton without replacing the ancestor", async () => {
    const listing = deferred();
    handlers.cloud_object_list = ({ prefix }) => prefix ? listing.promise : page([{ key: 'docs/file.txt' }]);
    const wrapper = await browser('columns');
    await click(wrapper, '[data-key="docs/"]');
    expect(wrapper.findAll('.column')[1].find('.browser-skeleton').exists()).toBe(true);
    listing.reject(error('CLOUD_TRANSPORT'));
    await flushPromises();
    expect(wrapper.find('.column-view .browser-skeleton').exists()).toBe(false);
    expect(wrapper.findAll('.column')[0].find('[data-key="docs/"]').exists()).toBe(true);
  });
  it("does not restart column navigation on the second click or directory double click", async () => {
    const listing = deferred();
    handlers.cloud_object_list = ({ prefix }) => prefix ? listing.promise : page([{ key: 'docs/file.txt' }]);
    const wrapper = await browser('columns');
    const folder = wrapper.find('[data-key="docs/"]');
    folder.element.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 1 }));
    await flushPromises();
    folder.element.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 }));
    await flushPromises();
    await folder.trigger('dblclick');
    expect(nativeInvoke.mock.calls.filter(([name, args]) => name === 'cloud_object_list' && args.prefix === 'docs/')).toHaveLength(1);
    expect(wrapper.findAll('.column')).toHaveLength(2);
    listing.resolve(page([{ key: 'docs/file.txt' }])); await flushPromises();
    expect(wrapper.find('.selection-summary').text()).toContain('docs');
    expect(textButton(wrapper, t('peer.open'))).toBeUndefined();
    expect(wrapper.find('.selection-actions [data-test=download]').classes()).not.toContain('el-button--primary');
    await click(wrapper, '[aria-label="List"]');
    expect(textButton(wrapper, t('peer.open'))).toBeDefined();
  });
  it("shows ancestor rebuilding as loading, recovers failed ancestors and invalidates a connection switch", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([{ key: 'docs/file.txt' }]) : page([{ key: 'docs/file.txt' }]);
    const wrapper = await browser();
    await wrapper.find('[data-test=folder-row]').trigger('dblclick'); await flushPromises();
    const ancestor = deferred();
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([{ key: 'docs/file.txt' }]) : ancestor.promise;
    await click(wrapper, '[aria-label="Columns"]');
    expect(wrapper.find('.column-loading').exists()).toBe(true);
    expect(wrapper.find('.column-empty').exists()).toBe(false);
    ancestor.reject(error('CLOUD_TRANSPORT')); await flushPromises();
    expect(wrapper.find('.column-empty').exists()).toBe(false);
    handlers.cloud_object_list = ({ prefix }) => page([{ key: prefix ? 'docs/file.txt' : 'docs/file.txt' }]);
    await click(wrapper, '[data-test=retry-load]');
    expect(wrapper.findAll('.column')[0].text()).toContain('docs');
    const obsolete = deferred(); handlers.cloud_object_list = () => obsolete.promise;
    await click(wrapper, '[data-key="docs/"]');
    handlers.cloud_object_list = () => page([{ key: 'other.txt' }]);
    await wrapper.setProps({ connectionId: 'c2' }); await flushPromises();
    obsolete.resolve(page([{ key: 'docs/stale.txt' }])); await flushPromises();
    expect(wrapper.find('.column-view').text()).toContain('other.txt');
    expect(wrapper.text()).not.toContain('stale.txt');
  });
  it.each([
    { sizes: [7, 3], phases: ['completed', 'running'], percent: 70, bytes: '7 B / 10 B' },
    { sizes: [0, 0], phases: ['completed', 'running'], percent: 50 },
    { sizes: [7, 3], phases: ['failed', 'cancelled'], percent: 0, bytes: '0 B / 10 B' },
    { sizes: [0, 0], phases: ['completed', 'completed'], percent: 100 },
  ])("shows file statuses without inventing cloud byte progress: $phases / $sizes", async ({ sizes, phases }) => {
    handlers.cloud_object_list = () => page([{ ...file, size: sizes[0] }, { key: 'b.txt', size: sizes[1] }]);
    handlers.transfer_page_download = () => ({ items: ['a.txt', 'b.txt'].map((path, index) => ({ remotePath: path, relativePath: path, phase: phases[index] === 'cancelled' ? 'canceled' : phases[index] })), nextOffset: null });
    const wrapper = await browser();
    await wrapper.findAll('tbody tr')[0].trigger('click'); await wrapper.findAll('tbody tr')[1].trigger('click', { ctrlKey: true });
    await click(wrapper, '.selection-actions [data-test=download]');
    expect(wrapper.find('[role=progressbar]').exists()).toBe(false);
    expect(wrapper.find('[data-test=download-dialog] [data-test=progress]').exists()).toBe(false);
    expect(wrapper.findAll('.download-queue li')).toHaveLength(2);
    expect(wrapper.findAll('.download-queue li').map((row) => row.attributes('data-status'))).toEqual(phases.map((phase) => ({ completed: 'done', running: 'downloading', failed: 'error', cancelled: 'canceled' })[phase]));
  });
  it("invalidates pending ancestor rebuilds when switching buckets", async () => {
    handlers.cloud_object_list = () => page([{ key: 'docs/file.txt' }]);
    const wrapper = await browser();
    await wrapper.find('[data-test=folder-row]').trigger('dblclick'); await flushPromises();
    const ancestor = deferred();
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([{ key: 'docs/file.txt' }]) : ancestor.promise;
    await click(wrapper, '[aria-label="Columns"]');
    expect(wrapper.find('.column-loading').exists()).toBe(true);
    handlers.cloud_object_list = () => page([{ key: 'photos.jpg' }]);
    await wrapper.findAll('.bucket-link')[1].trigger('click'); await flushPromises();
    ancestor.resolve(page([{ key: 'stale/old.txt' }])); await flushPromises();
    expect(wrapper.findAll('.column')).toHaveLength(1);
    expect(wrapper.find('.column-view').text()).toContain('photos.jpg');
    expect(wrapper.text()).not.toContain('stale');
  });
  it("updates completed bytes from polled terminal statuses without inventing running progress", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 3 }]);
    handlers.cloud_object_download_batch = ({ items }) => ({ transfers: items.map((item, index) => transfer('running', { transferId: `t${index}`, key: item.key, direction: 'download' })) });
    const wrapper = await browser();
    await wrapper.findAll('tbody tr')[0].trigger('click'); await wrapper.findAll('tbody tr')[1].trigger('click', { ctrlKey: true });
    await click(wrapper, '.selection-actions [data-test=download]');
    expect(wrapper.find('[role=progressbar]').exists()).toBe(false);
    handlers.transfer_page_download = () => ({ items: [{ remotePath: 'a.txt', relativePath: 'a.txt', phase: 'completed' }, { remotePath: 'b.txt', relativePath: 'b.txt', phase: 'running' }], nextOffset: null });
    await vi.advanceTimersByTimeAsync(1500); await flushPromises();
    expect(wrapper.find('[role=progressbar]').exists()).toBe(false);
    expect(wrapper.findAll('.download-queue li')[0].attributes('data-status')).toBe('done');
    expect(wrapper.findAll('.download-queue li')[1].attributes('data-status')).toBe('downloading');
  });
  it("keeps queue CSS contracts in the queue dialogs without broad paragraph overrides", () => {
    // 队列样式随对话框拆到了组件里，契约跟着搬到真正的持有者；两个队列滚动区高度保持一致。
    expect(downloadDialogSource).toContain('max-height: min(360px, 45vh)');
    expect(uploadDialogSource).toContain('max-height: min(360px, 45vh)');
    expect(downloadDialogSource).toContain('overscroll-behavior: contain');
    expect(uploadDialogSource).toContain('overscroll-behavior: contain');
    // 旧的内联队列样式不应在文件面板里复活。
    expect(cloudBrowserSource).not.toContain('.transfer-queue');
    expect(cloudBrowserSource).not.toMatch(/\.cloud-browser p\s*[,\{]/);
    expect(cloudBrowserSource).not.toMatch(/@media \(max-width: 480px\)\s*\{\s*\.browser-main/);
    expect(cloudBrowserSource).toContain('display: flex; flex-direction: column; user-select: none');
  });
  it('uses a sidebar and main pane with a persistent create/connect entry', async () => {
    const wrapper = await browser();
    expect(wrapper.find('.browser-sidebar').exists()).toBe(true); expect(wrapper.find('.browser-main').exists()).toBe(true);
    expect(wrapper.find('.sync-management-entry').exists()).toBe(false); expect(wrapper.find('select').exists()).toBe(false);
    expect(wrapper.find('.bucket-link').classes()).toContain('is-active'); expect(wrapper.find('.sidebar-empty').text()).toBe(t('cloudBrowser.noSyncDrives'));
    expect(wrapper.find('.add-sync-drive').text()).toBe('>');
    await click(wrapper, '.add-sync-drive'); expect(wrapper.find('[data-test="sync-manager"]').text()).toContain('c1');
    expect(wrapper.find('[data-test="upload"]').exists()).toBe(false); expect(wrapper.text()).not.toMatch(/CAS|Append V2/);
    await wrapper.setProps({ connectionId: 'c2' }); await flushPromises();
    expect(wrapper.find('[data-test="sync-manager"]').exists()).toBe(false); expect(wrapper.find('[data-test="upload"]').exists()).toBe(true);
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'cloud_sync' || name === 'cloud_create_repository')).toBe(false);
  });
  it('numbers current-connection drives in manager order, highlights focus and returns to all files without preparing again', async () => {
    handlers.cloud_list = () => ({ connections: [{ id: 'c1', name: 'Main', bucket: 'main', buckets: ['main'] }], repositories: [{ id: 'cas', connectionId: 'c1' }, { id: 'other', connectionId: 'c2' }], appendRepositories: [{ connectionId: 'c1', binding: { repoId: 'append' } }] });
    const wrapper = await browser();
    expect(wrapper.findAll('.drive-link').map((item) => item.text())).toEqual(['Sync drive 1', 'Sync drive 2']);
    await wrapper.findAll('.drive-link')[1].trigger('click'); await flushPromises();
    expect(wrapper.findComponent('[data-test="sync-manager"]').props('focusId')).toBe('append');
    expect(wrapper.findAll('.drive-link')[1].classes()).toContain('is-active');
    await click(wrapper, '.back-to-files'); expect(wrapper.find('.bucket-link').classes()).toContain('is-active');
    await wrapper.findAll('.drive-link')[0].trigger('click'); await flushPromises();
    expect(wrapper.findComponent('[data-test="sync-manager"]').props('focusId')).toBe('cas');
    await click(wrapper, '.back-to-files');
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_prepare')).toHaveLength(1);
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_list')).toHaveLength(1);
  });
  it('refreshes sidebar after manager changes and drops a removed active drive', async () => {
    const catalog = { connections: [{ id: 'c1', name: 'Main', bucket: 'main', buckets: ['main'] }], repositories: [], appendRepositories: [] };
    handlers.cloud_list = () => structuredClone(catalog);
    const wrapper = await browser(); await click(wrapper, '.add-sync-drive');
    catalog.appendRepositories.push({ connectionId: 'c1', binding: { repoId: 'new' } });
    await click(wrapper, '[data-test="catalog-changed"]'); expect(wrapper.find('.drive-link').text()).toBe('Sync drive 1');
    await click(wrapper, '.drive-link'); catalog.appendRepositories = [];
    await click(wrapper, '[data-test="catalog-changed"]');
    expect(wrapper.find('.drive-link').exists()).toBe(false); expect(wrapper.find('.bucket-link').classes()).toContain('is-active');
    expect(wrapper.find('[data-test="upload"]').exists()).toBe(true);
  });
  it('routes catalog errors through the shared application failure handler', async () => {
    const failure = error('CLOUD_STORAGE', 'Catalog unavailable'); handlers.cloud_list = () => { throw failure; };
    await browser(); expect(app.handleFailure).toHaveBeenCalledWith(failure);
  });
  it("groups flat object keys as navigable virtual folders and keeps configured scope server-side", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([{ key: "docs/nested/a", size: 1 }]) : page([{ key: "docs/a", size: 1 }, file, { key: "docs/b", size: 2 }]);
    const wrapper = await browser(); expect(wrapper.findAll("[data-test=folder-row]")).toHaveLength(1); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "", next: null });
    await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
    await click(wrapper, '.breadcrumb'); expect(wrapper.find('.breadcrumb.is-current').text()).toBe('main');
    expect(objectChildren([{ key: "other/a" }], "docs/")).toEqual([]);
  });
  it("switches between icons, list, columns and gallery views", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix === "docs/" ? page([{ key: "docs/b.txt", size: 2 }]) : page([{ key: "docs/a.txt", size: 1 }, { key: "photo.jpg", size: 4 }]);
    const wrapper = await browser();
    expect(wrapper.findComponent({ name: 'FileManagerView' }).exists()).toBe(true);
    expect(wrapper.find(".file-table").exists()).toBe(true);
    await click(wrapper, '[aria-label="Icons"]');
    expect(wrapper.findAll(".file-tile")).toHaveLength(2);
    await click(wrapper, '[aria-label="Gallery"]');
    expect(wrapper.findAll(".gallery-thumb")).toHaveLength(2);
    await click(wrapper, '[aria-label="Columns"]');
    expect(wrapper.findAll(".column")).toHaveLength(1);
    await click(wrapper, ".column-row");
    await flushPromises();
    expect(wrapper.findAll(".column")).toHaveLength(2);
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
    wrapper.unmount();
  });
  it("shows incomplete pagination until the final page and merges duplicate keys", async () => {
    handlers.cloud_object_list = ({ next }) => next ? page([file, { key: "b.txt", size: 3 }]) : page([file], "cursor", false);
    const wrapper = await browser(); expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false); expect(app.setInfo).toHaveBeenCalledWith("cloudBrowser.incomplete");
    await click(wrapper, '[data-test=more]'); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "", next: "cursor" }); expect(wrapper.text()).not.toContain(t("cloudBrowser.incomplete")); expect(wrapper.findAll(".file-table tbody tr")).toHaveLength(2);
  });
  it("does not claim an empty or complete listing after loading failure", async () => { handlers.cloud_object_list = () => { throw error("CLOUD_TRANSPORT"); }; const wrapper = await browser(); await vi.advanceTimersByTimeAsync(300); await flushPromises(); expect(wrapper.text()).toContain("Original backend reason"); expect(wrapper.text()).not.toContain(t("cloudBrowser.empty")); expect(wrapper.find("[data-test=retry-load]").exists()).toBe(true); expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false); });
  it("never flashes hint text while the first page is still loading", async () => {
    const listing = deferred();
    const preparation = deferred();
    handlers.cloud_object_list = () => listing.promise;
    handlers.cloud_object_prepare = () => preparation.promise;
    const wrapper = await browser();
    expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain(t("cloudBrowser.incomplete"));
    expect(wrapper.text()).not.toContain(t("cloudBrowser.loading"));
    expect(wrapper.find(".pathbar-status").exists()).toBe(false);
    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    expect(wrapper.find(".pathbar-status").attributes('aria-label')).toBe(t('common.loading'));
    expect(wrapper.find(".pathbar-status").text()).toBe('');
    listing.resolve(page([file]));
    await flushPromises();
    expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain(t("cloudBrowser.loading"));
    preparation.resolve({ discovered: true });
    await flushPromises();
    expect(wrapper.find(".pathbar-status").exists()).toBe(false);
  });
  it("only warns about a partial listing once more objects are actually pending", async () => {
    handlers.cloud_object_list = () => page([file], "cursor", false);
    const wrapper = await browser();
    expect(app.setInfo).toHaveBeenCalledWith("cloudBrowser.incomplete");
    expect(wrapper.find('[data-test=incomplete]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain(t("cloudBrowser.incomplete"));
  });
  it("shows true empty state only after complete listing", async () => { handlers.cloud_object_list = () => page(); const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.empty")); });
  it("automatically checks protection and requires discovered true before edits", async () => {
    handlers.cloud_object_prepare = () => ({ physicalId: 'physical', discovered: false });
    const wrapper = await browser();
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_prepare', { connectionId: 'c1' });
    expect(wrapper.find('[data-test=upload]').attributes('disabled')).toBeDefined();
    expect(wrapper.find('[data-test=prepare]').exists()).toBe(false);
    expect(wrapper.find('input[aria-label="Object name (default: selected filename)"]').exists()).toBe(false);
    expect(wrapper.text()).not.toMatch(/physical|identity/i);
    handlers.cloud_object_prepare = () => ({ discovered: true });
    await click(wrapper, '[data-test=recheck]');
    expect(wrapper.find('[data-test=upload]').attributes('disabled')).toBeUndefined();
    expect(wrapper.find('.sidebar-identity strong').text()).toBe('Main');
    expect(wrapper.find('.cloud-transfers').exists()).toBe(false);
  });
  it("keeps browsing available after preparation failure and shows only the original reason", async () => {
    handlers.cloud_object_prepare = () => { throw error('CLOUD_STORAGE', 'Storage permission denied'); };
    const wrapper = await browser();
    expect(wrapper.find('.cloud-error__message').text()).toBe('Storage permission denied');
    expect(wrapper.text()).toContain('CLOUD_STORAGE');
    expect(wrapper.find('[data-test=upload]').attributes('disabled')).toBeDefined();
    expect(wrapper.find('[data-test=delete-prefix]').attributes('disabled')).toBeDefined();
    await click(wrapper, '[data-test=file-row]');
    expect(textButton(wrapper, 'Download').attributes('disabled')).toBeUndefined();
    await click(wrapper, '[data-test=refresh]');
    expect(wrapper.find('.cloud-error__message').text()).toBe('Storage permission denied');
  });
  it("honors protected error codes and displays original message without matching text", async () => {
    handlers.cloud_object_select_upload_directory = () => { throw error("CLOUD_OBJECT_PROTECTED", "anything at all"); }; const wrapper = await browser(); await prepare(wrapper); await click(wrapper, '[data-test=upload]'); expect(wrapper.text()).toContain("anything at all"); expect(errorStrategy(error("CLOUD_DELETE_PLAN_STALE"))).toBe("retry");
  });
  it("selects single uploads through the bounded queue and starts Ask without inferring authorization from ETag", async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=upload]');
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_select_upload_directory', { connectionId: 'c1', prefix: '', mode: 'single' });
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_start_upload_directory', { connectionId: 'c1', batchId: 'batch', conflictPolicy: 'ask' });
    expect(confirmAction).not.toHaveBeenCalled();
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'cloud_object_upload' || name === 'cloud_object_select_local')).toBe(false);
  });
  it("captures the navigated prefix for a single upload without repeating preparation", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix ? page([]) : page([{ key: 'docs/old.txt', size: 1 }]);
    const wrapper = await browser(); await wrapper.find('[data-test=folder-row]').trigger('dblclick'); await flushPromises(); await click(wrapper, '[data-test=upload]');
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_select_upload_directory', { connectionId: 'c1', prefix: 'docs/', mode: 'single' });
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_prepare')).toHaveLength(1);
  });
  it("keeps single-upload conflicts unchecked and uses dedicated cancellation", async () => {
    handlers.cloud_object_start_upload_directory = () => uploadSummary('conflict', { conflict: { conflictId: 'one', relativePath: 'a.txt', etag: 'v1' } });
    const wrapper = await browser(); await click(wrapper, '[data-test=upload]');
    expect(findInDocument(wrapper, '[data-test=upload-apply-remaining]').element.checked).toBe(false);
    await click(wrapper, '[data-test=upload-conflict-cancel]');
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_cancel_upload_directory', { connectionId: 'c1', batchId: 'batch' });
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'cloud_object_resolve_upload_directory_conflict')).toBe(false);
  });
  it("downloads single files through an opaque native queue, asks twice by default and reveals the exact queue", async () => {
    let state = queueSummary('conflict', { conflict: { conflictId: 'first', relativePath: 'a.txt' } });
    handlers.transfer_start_download = handlers.transfer_status_download = () => state;
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_select_download', { backend: { backend: 's3', connectionId: 'c1' }, selection: { mode: 'single', path: 'a.txt' } });
    expect(findInDocument(wrapper, '[data-test=download-apply-remaining]').element.checked).toBe(false);
    state = queueSummary('conflict', { skippedFiles: 1, conflict: { conflictId: 'second', relativePath: 'b.txt' } });
    await click(wrapper, '[data-test=download-skip]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_resolve_download', { queueId: 'queue-1', conflictId: 'first', decision: 'skip', applyToRemaining: false });
    expect(findInDocument(wrapper, '[data-test=download-apply-remaining]').element.checked).toBe(false);
    await findInDocument(wrapper, '[data-test=download-apply-remaining]').setValue(true);
    state = queueSummary('completed', { completedFiles: 1, skippedFiles: 1 });
    await click(wrapper, '[data-test=download-overwrite]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_resolve_download', { queueId: 'queue-1', conflictId: 'second', decision: 'overwrite', applyToRemaining: true });
    await click(wrapper, '[data-test=reveal-download]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_reveal_download', { queueId: 'queue-1' });
    await click(wrapper, '[data-test=close-download]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_release_download', { queueId: 'queue-1' });
    expect(confirmAction).not.toHaveBeenCalled();
  });
  it('shows backend failures without turning preconditions into overwrite permission', async () => {
    handlers.transfer_start_download = () => queueSummary('failed', { error: error('CLOUD_PRECONDITION', 'size changed') });
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    expect(wrapper.find('[data-test=download-dialog]').text()).toContain('size changed');
    expect(wrapper.find('[data-test=download-overwrite]').exists()).toBe(false);
    expect(confirmAction).not.toHaveBeenCalled();
  });
  it.each(['connection', 'bucket', 'unmount'])('releases late selected queue and never starts it after %s changes', async (change) => {
    const selected = deferred(); handlers.transfer_select_download = () => selected.promise;
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    if (change === 'connection') await wrapper.setProps({ connectionId: 'c2' });
    else if (change === 'bucket') await wrapper.findAll('.bucket-link')[1].trigger('click');
    else wrapper.unmount();
    await flushPromises(); selected.resolve(queueSummary('selected')); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_release_download', { queueId: 'queue-1' });
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'transfer_start_download')).toBe(false);
  });
  it('guards double-click selection and keeps cancellation running until native settlement', async () => {
    const selected = deferred(); handlers.transfer_select_download = () => selected.promise;
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]');
    await textButton(wrapper, 'Download').trigger('click');
    expect(textButton(wrapper, 'Download')).toBeUndefined();
    // A deliberate reselection while the picker is pending still cannot open a second picker.
    await click(wrapper, '[data-test=file-row]');
    await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'transfer_select_download')).toHaveLength(1);
    selected.resolve(queueSummary('selected')); await flushPromises();
    const stopped = deferred(); handlers.transfer_cancel_download = () => stopped.promise;
    await click(wrapper, '[data-test=cancel-download]');
    expect(wrapper.find('[data-test=close-download]').exists()).toBe(false);
    expect(wrapper.find('[data-test=cancel-download]').attributes('disabled')).toBeDefined();
    handlers.transfer_status_download = () => queueSummary('canceled'); stopped.resolve(null); await flushPromises();
    expect(wrapper.find('[data-test=close-download]').exists()).toBe(true);
  });
  it('does not let old queue polls resurrect old state after cancel and a new selection', async () => {
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    const stale = deferred(); handlers.transfer_status_download = () => stale.promise;
    await vi.advanceTimersByTimeAsync(1500);
    handlers.transfer_cancel_download = () => null; handlers.transfer_status_download = () => queueSummary('canceled');
    await click(wrapper, '[data-test=cancel-download]'); await click(wrapper, '[data-test=close-download]');
    handlers.transfer_select_download = () => queueSummary('selected', { queueId: 'queue-2' });
    handlers.transfer_start_download = () => queueSummary('completed', { queueId: 'queue-2', completedFiles: 1 });
    await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    stale.resolve(queueSummary('failed', { error: error('OLD', 'obsolete failure') })); await flushPromises();
    expect(wrapper.text()).not.toContain('obsolete failure');
    await click(wrapper, '[data-test=reveal-download]');
    expect(nativeInvoke).toHaveBeenLastCalledWith('transfer_reveal_download', { queueId: 'queue-2' });
  });
  it('retains unknown upload recovery separately without acknowledging it when closing a queue', async () => {
    handlers.cloud_object_status = () => [transfer('unknown', { transferId: 'upload-unknown', recoveryRequired: true })];
    handlers.transfer_start_download = () => queueSummary('completed', { completedFiles: 1 });
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, 'Download').trigger('click'); await flushPromises();
    await click(wrapper, '[data-test=close-download]');
    expect(wrapper.find('.cloud-transfers').text()).toContain(t('cloudBrowser.recovery'));
    expect(nativeInvoke.mock.calls.some(([name]) => name === 'cloud_object_recovery_acknowledge')).toBe(false);
  });
  it("shows cancellation acknowledgement and unknown recovery without fake percentages", async () => {
    handlers.cloud_object_status = () => [transfer()]; const wrapper = await browser(); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_cancel", { transferId: "t1" }); expect(wrapper.text()).toContain(t("cloudBrowser.phase.cancelling")); handlers.cloud_object_status = () => [transfer("unknown", { recoveryRequired: true })]; await vi.advanceTimersByTimeAsync(1500); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.unknown")); expect(wrapper.text()).toContain(t("cloudBrowser.recovery")); expect(wrapper.html()).not.toContain("progressbar"); expect(wrapper.text()).not.toContain("%");
  });
  it("refuses to delete the bucket root and requires deleting entries individually", async () => {
    const wrapper = await browser(); await prepare(wrapper);
    await click(wrapper, '[data-test=delete-prefix]');
    expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_plan")).toBe(false);
    expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false);
    expect(wrapper.text()).toContain(t("cloudBrowser.deleteRootForbidden"));
  });
  it("plans deletion before confirming exact target prefix count and irreversible warning", async () => {
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]);
    const docsEntries = Array.from({ length: 5 }, (_, i) => ({ key: `docs/exact-${i}.txt`, size: i, etag: null }));
    handlers.cloud_object_delete_plan = () => deletePlan({ entries: docsEntries, objectCount: docsEntries.length });
    handlers.cloud_object_delete_execute = () => ({ deleted: ["a"], failed: [{ key: "b", error: error("CLOUD_STORAGE") }] }); const wrapper = await browser(); await prepare(wrapper); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); await click(wrapper, '[data-test=delete-prefix]'); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_delete_plan", { connectionId: "c1", prefix: "docs/" }); expect(confirmAction.mock.calls[0][1]).toBe(""); expect(confirmAction.mock.calls[0][2]).toMatchObject({ headerless: true, danger: true, type: null, confirmText: t("common.delete") }); const review = mount({ render: () => confirmAction.mock.calls[0][0] }); expect(review.find(".confirm-heading").text()).toBe(t("cloudBrowser.deleteTitle")); expect(review.find("svg").attributes("width")).toBe("20"); expect(review.find("svg circle").attributes("fill")).toBe("currentColor"); expect(review.find(".confirm-heading__icon").exists()).toBe(true); expect(review.find(".confirm-summary").text()).toBe(t("cloudBrowser.deleteSummary", { count: 5 })); expect(review.find('[data-test=delete-target]').text()).toBe("docs/"); expect(review.find('[data-test=delete-keys]').element.classList.contains("confirm-list")).toBe(true); expect(review.findAll("li").map((item) => item.text())).toEqual(docsEntries.map((item) => item.key)); review.unmount(); expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_delete_execute", { connectionId: "c1", planId: "plan-1", confirmed: true }); expect(app.setError).toHaveBeenCalledWith("cloudBrowser.deletePartial", { deleted: 1, failed: 1, reason: "Original backend reason" }); expect(wrapper.text()).not.toContain("Deleted: 1; failed: 1");
  });
  it("shows a loading delete button while the plan executes and never a persistent result banner", async () => {
    const execute = deferred();
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]);
    handlers.cloud_object_delete_plan = () => deletePlan({ entries: [{ key: "docs/a", size: 1, etag: null }], objectCount: 1 });
    handlers.cloud_object_delete_execute = () => execute.promise;
    const wrapper = await browser(); await prepare(wrapper);
    await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises();
    await click(wrapper, '[data-test=delete-prefix]');
    expect(wrapper.find('[data-test=delete-prefix]').attributes("disabled")).toBeDefined();
    expect(wrapper.find('[data-test=delete-prefix] .action-spinner').exists()).toBe(true);
    execute.resolve({ deleted: ["a", "b"], failed: [] }); await flushPromises();
    expect(app.setNotice).toHaveBeenCalledWith("cloudBrowser.deletedObjects", { count: 2 });
    expect(wrapper.find('[data-test=delete-prefix] .action-spinner').exists()).toBe(false);
    expect(wrapper.text()).not.toContain(t("cloudBrowser.deletedObjects", { count: 2 }));
  });
  it("does not execute cancelled or expired delete plans", async () => { handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]); const docsPlan = { entries: [{ key: "docs/a", size: 1, etag: null }], objectCount: 1 }; handlers.cloud_object_delete_plan = () => deletePlan(docsPlan); const wrapper = await browser(); await prepare(wrapper); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); confirmAction.mockResolvedValue(false); await click(wrapper, '[data-test=delete-prefix]'); confirmAction.mockResolvedValue(true); handlers.cloud_object_delete_plan = () => deletePlan({ planId: "expired", expiresAtUnix: 1, ...docsPlan }); await click(wrapper, '[data-test=delete-prefix]'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false); expect(wrapper.text()).toContain(t('cloudBrowser.stalePlan')); });
  it("ignores stale directory page results after navigation", async () => { const late = deferred(); let count = 0; handlers.cloud_object_list = ({ prefix }) => { if (prefix) return page([{ key: "docs/new", size: 1 }]); if (++count === 1) return page([{ key: "docs/old", size: 1 }], "next", false); return late.promise; }; const wrapper = await browser(); await wrapper.find('[data-test=more]').trigger("click"); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); late.resolve(page([{ key: "stale", size: 2 }])); await flushPromises(); expect(wrapper.text()).toContain("new"); expect(wrapper.text()).not.toContain("stale"); });
  it("ignores pending preparation after a connection change", async () => { const late = deferred(); handlers.cloud_object_prepare = ({ connectionId }) => connectionId === 'c1' ? late.promise : { discovered: false }; const wrapper = await browser(); await vi.advanceTimersByTimeAsync(300); await flushPromises(); expect(wrapper.find('.pathbar-status').attributes('aria-label')).toBe(t('common.loading')); await wrapper.setProps({ connectionId: 'c2' }); await flushPromises(); late.resolve({ discovered: true }); await flushPromises(); expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_prepare', { connectionId: 'c2' }); expect(wrapper.find('[data-test=upload]').attributes('disabled')).toBeDefined(); });
  it("cleans polling on unmount and ignores in-flight poll completion", async () => { const late = deferred(); handlers.cloud_object_status = () => late.promise; const wrapper = await browser(); wrapper.unmount(); late.resolve([transfer()]); await flushPromises(); await vi.advanceTimersByTimeAsync(4500); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_status")).toHaveLength(1); expect(vi.getTimerCount()).toBe(0); });
  it("does not execute a deletion confirmed after directory switch", async () => {
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]); handlers.cloud_object_delete_plan = () => deletePlan({ entries: [{ key: "docs/a", size: 1, etag: null }], objectCount: 1 }); const answer = deferred(); confirmAction.mockImplementation(() => answer.promise); const wrapper = await browser(); await prepare(wrapper); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); await wrapper.find('[data-test=delete-prefix]').trigger("click"); await flushPromises(); await wrapper.findAll(".breadcrumb")[0].trigger("click"); await flushPromises(); answer.resolve(true); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false);
  });
  it("ignores stale catalog failures after changing connections", async () => {
    const late = deferred(); let count = 0; handlers.cloud_list = () => ++count === 1 ? late.promise : { connections: [{ id: "c2", name: "Other" }] }; const wrapper = await browser(); await wrapper.setProps({ connectionId: "c2" }); await flushPromises(); late.reject(error("CLOUD_TRANSPORT", "obsolete catalog error")); await flushPromises(); expect(wrapper.text()).not.toContain("obsolete catalog error");
  });
  it("does not let an earlier poll overwrite a cancellation response", async () => {
    handlers.cloud_object_status = () => [transfer()]; const wrapper = await browser(); const late = deferred(); handlers.cloud_object_status = () => late.promise; await vi.advanceTimersByTimeAsync(1500); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); late.resolve([transfer()]); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.cancelling")); expect(wrapper.text()).not.toContain(t("cloudBrowser.cancel"));
  });
  it("renders cancelled and failed terminal states without a cancellation action", async () => {
    handlers.cloud_object_status = () => [transfer("cancelled"), transfer("failed", { transferId: "t2", error: error("CLOUD_STORAGE") })]; const wrapper = await browser(); expect(wrapper.text()).not.toContain(t("cloudBrowser.phase.cancelled")); expect(wrapper.text()).toContain(t("cloudBrowser.phase.failed")); expect(wrapper.text()).toContain("Original backend reason"); expect(textButton(wrapper, t("cloudBrowser.cancel"))).toBeUndefined();
  });
  it("whitelists all object payloads and strips arbitrary paths and association fields", async () => {
    handlers.cloud_object_download_batch = () => ({ transfers: [], localRoot: "" });
    const args = { connectionId: "c1", localToken: "opaque", key: "a", fileName: "a", path: "/secret", localPath: "/secret", extra: true };
    await cloudObjects.upload(args); await cloudObjects.download(args); await cloudObjects.downloadBatch({ ...args, items: [] }); await cloudObjects.prepare(args); await cloudObjects.list(args); await cloudObjects.discover(args); await cloudObjects.selectLocal({ ...args, direction: "upload" }); await cloudObjects.deletePlan(args); await cloudObjects.deleteExecute({ ...args, planId: "p", confirmed: true }); await cloudObjects.recoveryAcknowledge({ ...args, transferId: "t1", connectionRevision: 1, confirmed: true });
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
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]);
    handlers.cloud_object_delete_plan = () => deletePlan(extra); const wrapper = await browser(); await prepare(wrapper); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); await click(wrapper, '[data-test=delete-prefix]');
    expect(confirmAction).not.toHaveBeenCalled(); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_delete_execute")).toBe(false); expect(wrapper.text()).toContain(t("cloudBrowser.invalidPlan"));
  });
  it("renders every exact key in a long scrollable confirmation, preserving escaped characters", async () => {
    const entries = Array.from({ length: 130 }, (_, i) => ({ key: `docs/folder/key-${i}\nquoted\"`, size: i, etag: null }));
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }]);
    handlers.cloud_object_delete_plan = () => deletePlan({ entries, objectCount: entries.length }); confirmAction.mockResolvedValue(false);
    const wrapper = await browser(); await prepare(wrapper); await wrapper.find("[data-test=folder-row]").trigger("dblclick"); await flushPromises(); await click(wrapper, '[data-test=delete-prefix]'); const review = mount({ render: () => confirmAction.mock.calls[0][0] });
    expect(review.text()).toContain("130"); expect(review.findAll("li").map((item) => item.text())).toEqual(entries.map((item) => JSON.stringify(item.key))); expect(review.find("ul").element.classList.contains("confirm-list")).toBe(true); review.unmount();
  });
  it("uses prefix protection for read-only controls while allowing raw download", async () => {
    handlers.cloud_object_list = () => ({ ...page([file]), prefixProtected: true }); const wrapper = await browser();
    expect(wrapper.find('[data-test=prepare]').exists()).toBe(false); expect(wrapper.find('[data-test=upload]').attributes("disabled")).toBeDefined(); expect(wrapper.find('[data-test=delete-prefix]').attributes("disabled")).toBeDefined();
    await click(wrapper, '[data-test=file-row]'); await textButton(wrapper, t("cloudBrowser.download")).trigger("click"); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "transfer_select_download")).toBe(true);
  });
  it("blocks protected single objects, ancestor-prefix deletion and protected overwrite, not downloads", async () => {
    handlers.cloud_object_list = () => page([{ ...file, protected: true }, { key: "docs/private/a", protected: true, size: 1 }, { key: "docs/public", protected: false, size: 1 }]);
    const wrapper = await browser(); await prepare(wrapper); expect(wrapper.find('[data-test=delete-prefix]').attributes("disabled")).toBeDefined();
    await click(wrapper, '[data-test=file-row]'); expect(wrapper.find('[data-test=delete-file]').attributes("disabled")).toBeDefined();
    await click(wrapper, '[data-test=upload]'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_upload")).toBe(false);
    expect(wrapper.find('input[aria-label="Object name (default: selected filename)"]').exists()).toBe(false);
    await textButton(wrapper, t("cloudBrowser.download")).trigger("click"); await flushPromises(); expect(nativeInvoke.mock.calls.some(([name]) => name === "transfer_select_download")).toBe(true);
  });
  it.each(["en", "zh-CN"])("requires explicit acknowledgement and explains local-only risk in %s", async (locale) => {
    setLocale(locale); handlers.cloud_object_status = () => [transfer("unknown", { key: "uncertain/key", recoveryRequired: true })]; const wrapper = await browser();
    expect(wrapper.text()).toContain('"uncertain/key"'); expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_recovery_acknowledge")).toBe(false);
    confirmAction.mockResolvedValue(false); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); expect(confirmAction.mock.calls[0][1]).toBe(t("cloudBrowser.acknowledge")); expect(confirmAction.mock.calls[0][2]).toMatchObject({ confirmText: t("cloudBrowser.acknowledgeAction") }); const ack = mount({ render: () => confirmAction.mock.calls[0][0] }); expect(ack.find(".confirm-summary").text()).toBe(t("cloudBrowser.acknowledgeBody")); expect(ack.find(".confirm-scope__value").text()).toBe("uncertain/key"); ack.unmount();
    expect(nativeInvoke.mock.calls.some(([name]) => name === "cloud_object_recovery_acknowledge")).toBe(false);
    confirmAction.mockResolvedValue(true); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_recovery_acknowledge", { transferId: "t1", connectionId: "c1", connectionRevision: 1, confirmed: true });
    expect(wrapper.text()).toContain(t("cloudBrowser.acknowledgedNotice")); expect(textButton(wrapper, t("cloudBrowser.acknowledge"))).toBeUndefined(); expect(wrapper.text()).not.toContain(t("cloudBrowser.phase.completed"));
  });
  it("does not infer a legacy null transfer target", async () => {
    handlers.cloud_object_status = () => [transfer("unknown", { key: null, sizeHint: null })]; const wrapper = await browser(); expect(wrapper.text()).toContain(t("cloudBrowser.unknownTarget"));
    await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); const ack = mount({ render: () => confirmAction.mock.calls[0][0] }); expect(ack.find('.confirm-scope__value').text()).toBe(t("cloudBrowser.unknownTarget")); expect(ack.text()).not.toContain("a.txt"); ack.unmount();
  });
  it("prevents a stale poll from overwriting acknowledgement and ignores confirmation after route change", async () => {
    handlers.cloud_object_status = () => [transfer("unknown")]; const wrapper = await browser(); const late = deferred(); handlers.cloud_object_status = () => late.promise;
    await vi.advanceTimersByTimeAsync(1500); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); late.resolve([transfer("unknown")]); await flushPromises(); expect(wrapper.text()).toContain(t("cloudBrowser.phase.acknowledged"));
    handlers.cloud_object_status = () => [transfer("unknown")]; await vi.advanceTimersByTimeAsync(1500); const answer = deferred(); confirmAction.mockImplementation(() => answer.promise); await textButton(wrapper, t("cloudBrowser.acknowledge")).trigger("click"); await flushPromises(); await wrapper.setProps({ connectionId: "c2" }); await flushPromises(); answer.resolve(true); await flushPromises(); expect(nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_recovery_acknowledge")).toHaveLength(1);
  });
  it("shows stop timeout guidance without hiding the original error", async () => {
    handlers.cloud_object_status = () => [transfer()]; handlers.cloud_object_cancel = () => { throw error("CLOUD_OBJECT_STOP_TIMEOUT"); }; const wrapper = await browser(); await textButton(wrapper, t("cloudBrowser.cancel")).trigger("click"); await flushPromises(); expect(wrapper.text()).toContain("Original backend reason"); expect(wrapper.find('.cloud-error__message').text()).toBe('Original backend reason'); expect(wrapper.text()).toContain('CLOUD_OBJECT_STOP_TIMEOUT'); expect(errorStrategy(error("CLOUD_OBJECT_STOP_TIMEOUT"))).toBe("show");
  });
  // IPC contract guard only: backend prefix validation is covered by Rust tests.
  it.each([false, true])("keeps docs/ through folder deletion and waits for confirmation (%s)", async (accepted) => {
    const entries = [{ key: "docs/a", size: 1, etag: "etag" }, { key: "docs/nested/b", size: 1, etag: "etag" }];
    handlers.cloud_object_list = ({ prefix }) => page(prefix === "docs/" ? entries : [...entries, { key: "docs-sibling/keep", size: 1 }]);
    handlers.cloud_object_delete_plan = ({ connectionId, prefix }) => {
      expect(connectionId).toBe("c1");
      expect(prefix).toBe("docs/");
      return deletePlan({ objectCount: entries.length, entries });
    };
    const answer = deferred();
    confirmAction.mockImplementation(() => answer.promise);
    const executions = () => nativeInvoke.mock.calls.filter(([name]) => name === "cloud_object_delete_execute");
    const wrapper = await browser();
    await wrapper.find('[data-key="docs/"]').trigger("dblclick"); await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
    await click(wrapper, "[data-test=delete-prefix]");
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_delete_plan", { connectionId: "c1", prefix: "docs/" });
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(executions()).toHaveLength(0);
    answer.resolve(accepted); await flushPromises();
    expect(executions()).toHaveLength(accepted ? 1 : 0);
    if (accepted) expect(executions()[0][1]).toEqual({ connectionId: "c1", planId: "plan-1", confirmed: true });
  });
  it("renders PeerBrowser-style list headers and hides trailing folder slashes", async () => {
    handlers.cloud_object_list = () => page([{ key: "docs/a", size: 1 }, file]);
    const wrapper = await browser();
    expect(wrapper.findAll(".file-table thead th").map((item) => item.text())).toEqual([t("peer.name"), t("peer.kind"), t("peer.size")]);
    const folder = wrapper.find("[data-test=folder-row]");
    expect(folder.text()).toContain("docs");
    expect(folder.text()).not.toContain("docs/");
    expect(wrapper.find("[data-test=file-row]").text()).not.toContain("修改时间");
  });
  it("opens folders and previews previewable files on double click", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix
      ? page([{ key: "docs/a.txt", size: 4 }])
      : page([{ key: "docs/a.txt", size: 4 }, { key: "note.txt", size: 5 }]);
    handlers.cloud_object_preview = () => ({ fileName: "note.txt", mime: "text/plain", encoding: "utf8", data: "hello cloud", size: 11 });
    const wrapper = await browser();
    await wrapper.find("[data-test=folder-row]").trigger("dblclick");
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
    await wrapper.findAll(".breadcrumb")[0].trigger("click");
    await flushPromises();
    await wrapper.find("[data-test=file-row]").trigger("dblclick");
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_preview", { connectionId: "c1", key: "note.txt" });
    expect(wrapper.find(".preview-dialog").exists()).toBe(true);
    expect(wrapper.find(".preview-text-full").text()).toContain("hello cloud");
    await textButton(wrapper, t("common.close")).trigger("click");
    expect(wrapper.find(".preview-dialog").exists()).toBe(false);
  });
  it("shows folders in the gallery and previews media in the stage", async () => {
    handlers.cloud_object_list = () => page([{ key: "docs/a.txt", size: 1 }, { key: "photo.png", size: 4 }]);
    handlers.cloud_object_preview = () => ({ fileName: "photo.png", mime: "image/png", encoding: "base64", data: "AAAA", size: 4 });
    const wrapper = await browser();
    await click(wrapper, '[aria-label="Gallery"]');
    expect(wrapper.findAll(".gallery-thumb").map((item) => item.text())).toEqual(["docs", "photo.png"]);
    await wrapper.findAll(".gallery-thumb")[1].trigger("click");
    await flushPromises();
    expect(wrapper.find(".gallery-image").exists()).toBe(true);
    expect(wrapper.find(".preview-dialog").exists()).toBe(false);
    await wrapper.findAll(".gallery-thumb")[0].trigger("dblclick");
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith("cloud_object_list", { connectionId: "c1", prefix: "docs/", next: null });
  });
  it("keeps the current listing visible when refresh fails and retries on demand", async () => {
    const wrapper = await browser();
    expect(wrapper.findAll("[data-test=file-row]")).toHaveLength(1);
    handlers.cloud_object_list = () => { throw error("CLOUD_TRANSPORT"); };
    await click(wrapper, "[data-test=refresh]");
    expect(wrapper.text()).toContain("Original backend reason");
    expect(wrapper.findAll("[data-test=file-row]")).toHaveLength(1);
    expect(wrapper.find("[data-test=retry-load]").exists()).toBe(true);
    handlers.cloud_object_list = () => page([file, { key: "b.txt", size: 2 }]);
    await click(wrapper, "[data-test=retry-load]");
    expect(wrapper.findAll("[data-test=file-row]")).toHaveLength(2);
  });
  it("keeps the previous bucket listing when a bucket switch fails", async () => {
    handlers.cloud_object_select_bucket = () => { throw error("CLOUD_TRANSPORT"); };
    const wrapper = await browser();
    await wrapper.findAll(".bucket-link")[1].trigger("click");
    await flushPromises();
    expect(app.handleFailure).toHaveBeenCalled();
    expect(wrapper.find(".bucket-link.is-active").text()).toBe("main");
    expect(wrapper.findAll("[data-test=file-row]")).toHaveLength(1);
  });
  it("rebuilds every ancestor column after navigating deep", async () => {
    handlers.cloud_object_list = ({ prefix }) => {
      if (prefix === '') return page([{ key: 'docs/x', size: 0 }]);
      if (prefix === 'docs/') return page([{ key: 'docs/nested/y', size: 0 }]);
      if (prefix === 'docs/nested/') return page([{ key: 'docs/nested/c.txt', size: 3 }]);
      return page([]);
    };
    const wrapper = await browser();
    await wrapper.find('[data-test=folder-row]').trigger('dblclick');
    await flushPromises();
    await wrapper.find('[data-test=folder-row]').trigger('dblclick');
    await flushPromises();
    await click(wrapper, '[aria-label="Columns"]');
    await flushPromises();
    const columns = wrapper.findAll('.column');
    expect(columns).toHaveLength(3);
    expect(columns[0].text()).toContain('docs');
    expect(columns[1].text()).toContain('nested');
    expect(columns[2].text()).toContain('c.txt');
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_list', { connectionId: 'c1', prefix: '', next: null });
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_list', { connectionId: 'c1', prefix: 'docs/', next: null });
  });
  it("binds the backend session to the first bucket before the first listing", async () => {
    await browser();
    const selectIndex = nativeInvoke.mock.calls.findIndex(([name]) => name === 'cloud_object_select_bucket');
    const listIndex = nativeInvoke.mock.calls.findIndex(([name]) => name === 'cloud_object_list');
    expect(selectIndex).toBeGreaterThan(-1);
    expect(listIndex).toBeGreaterThan(selectIndex);
    expect(nativeInvoke).toHaveBeenCalledWith('cloud_object_select_bucket', { connectionId: 'c1', bucket: 'main' });
    expect(nativeInvoke.mock.calls[selectIndex][1]).toEqual({ connectionId: 'c1', bucket: 'main' });
    expect(selectIndex).toBeLessThan(listIndex);
  });
  it("selects multiple rows with ctrl and shift and batches their download", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }, { key: 'c.txt', size: 3 }]);
    handlers.cloud_object_download_batch = ({ items }) => ({ transfers: items.map((item, index) => transfer('running', { transferId: `t${index}`, direction: 'download', key: item.key })), localRoot: '/tmp/target' });
    const wrapper = await browser();
    const rows = wrapper.findAll('[data-test=file-row]');
    await rows[0].trigger('click');
    await rows[1].trigger('click', { ctrlKey: true });
    expect(wrapper.find('.selection-summary').text()).toContain(t('peer.selectedCount', { count: 2 }));
    // The range starts at the last clicked row, so b..c replaces the add-on.
    await rows[2].trigger('click', { shiftKey: true });
    expect(wrapper.find('.selection-summary').text()).toContain(t('peer.selectedCount', { count: 2 }));
    await textButton(wrapper, t('cloudBrowser.download')).trigger('click');
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_select_download', {
      backend: { backend: 's3', connectionId: 'c1' }, selection: { mode: 'files', paths: ['b.txt', 'c.txt'] },
    });
  });
  it("downloads a whole folder with its relative tree in one batch", async () => {
    handlers.cloud_object_list = ({ prefix }) => {
      if (!prefix) return page([{ key: 'docs/a.txt', size: 1 }]);
      if (prefix === 'docs/') return page([{ key: 'docs/a.txt', size: 1 }, { key: 'docs/deep/b.bin', size: 2 }]);
      if (prefix === 'docs/deep/') return page([{ key: 'docs/deep/b.bin', size: 2 }]);
      return page([]);
    };
    handlers.cloud_object_download_batch = ({ items }) => ({ transfers: items.map((item, index) => transfer('running', { transferId: `t${index}`, direction: 'download', key: item.key })), localRoot: '/tmp/target' });
    const wrapper = await browser();
    await wrapper.find('[data-test=folder-row]').trigger('click');
    await textButton(wrapper, t('cloudBrowser.download')).trigger('click');
    await flushPromises();
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_select_download', { backend: { backend: 's3', connectionId: 'c1' }, selection: { mode: 'folder', path: 'docs/' } });
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_list')).toHaveLength(1);
  });
  it("applies a batch overwrite decision to the native queue without a second selection", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }]);
    handlers.transfer_start_download = () => queueSummary('conflict', { conflict: { conflictId: 'batch-conflict', relativePath: 'a.txt' } });
    const wrapper = await browser(); await wrapper.findAll('[data-test=file-row]')[0].trigger('click'); await wrapper.findAll('[data-test=file-row]')[1].trigger('click', { ctrlKey: true });
    await click(wrapper, '[data-test=download]'); await findInDocument(wrapper, '[data-test=download-apply-remaining]').setValue(true); await click(wrapper, '[data-test=download-overwrite]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_resolve_download', { queueId: 'queue-1', conflictId: 'batch-conflict', decision: 'overwrite', applyToRemaining: true });
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'transfer_select_download')).toHaveLength(1);
    expect(confirmAction).not.toHaveBeenCalled();
  });
  it('reports failed direct selection without retrying the listing or implicitly overwriting', async () => {
    const failure = error('CLOUD_TRANSPORT'); handlers.transfer_select_download = () => { throw failure; };
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await click(wrapper, '[data-test=download]');
    expect(app.handleFailure).toHaveBeenCalledWith(failure);
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_list')).toHaveLength(1);
    expect(confirmAction).not.toHaveBeenCalled();
  });
  it.each(['CLOUD_PRECONDITION', 'TRANSFER_CONFLICT'])('keeps a terminal %s failure distinct from an actionable queue conflict', async (code) => {
    handlers.transfer_start_download = () => queueSummary('failed', { error: error(code) });
    const wrapper = await browser(); await click(wrapper, '[data-test=file-row]'); await click(wrapper, '[data-test=download]');
    expect(wrapper.find('[data-test=download-dialog]').text()).toContain('Original backend reason');
    expect(wrapper.find('[data-test=download-overwrite]').exists()).toBe(false);
    expect(wrapper.find('[data-test=retry-load]').exists()).toBe(false);
    expect(confirmAction).not.toHaveBeenCalled();
    await click(wrapper, '[data-test=close-download]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_release_download', { queueId: 'queue-1' });
  });
  it.each(['AUTH_INTERFACE_LOCKED', 'VAULT_LOCKED', 'AUTH_PASSWORD_CHANGE_REQUIRED'])('forwards %s from local queue failures to authentication handling', async (code) => {
    app.handleFailure.mockClear();
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }]);
    const failure = error(code);
    handlers.transfer_start_download = () => { throw failure; };
    const wrapper = await browser();
    await wrapper.findAll('[data-test=file-row]')[0].trigger('click');
    await wrapper.findAll('[data-test=file-row]')[1].trigger('click', { ctrlKey: true });
    await click(wrapper, '[data-test=download]');
    expect(app.handleFailure).toHaveBeenCalledWith(failure);
    expect(wrapper.find('[data-test=download-dialog]').text()).toContain(failure.message);
  });
  it("shows the download queue and reveals the finished destination", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }]);
    handlers.transfer_start_download = () => queueSummary('completed', { totalFiles: 2, completedFiles: 2 });
    handlers.transfer_page_download = () => ({ items: ['a.txt', 'b.txt'].map((path) => ({ remotePath: path, relativePath: path, phase: 'completed' })), nextOffset: null });
    const wrapper = await browser();
    await wrapper.find('[data-test=file-row]').trigger('click');
    await wrapper.findAll('[data-test=file-row]')[1].trigger('click', { ctrlKey: true });
    await textButton(wrapper, t('cloudBrowser.download')).trigger('click');
    await flushPromises();
    expect(wrapper.find('[data-test=download-dialog]').exists()).toBe(true);
    expect(wrapper.findAll('.download-queue li')).toHaveLength(2);
    expect(wrapper.findAll('.download-queue li').map((item) => item.text())).toEqual([
      expect.stringContaining(t('peer.downloadStatusDone')),
      expect.stringContaining(t('peer.downloadStatusDone')),
    ]);
    await click(wrapper, '[data-test=reveal-download]');
    expect(nativeInvoke).toHaveBeenCalledWith('transfer_reveal_download', { queueId: 'queue-1' });
  });
  it("keeps real reveal errors on the global failure path", async () => {
    handlers.cloud_object_list = () => page([file, { key: 'b.txt', size: 2 }]);
    handlers.cloud_object_download_batch = ({ items }) => ({ transfers: items.map((item, index) => transfer('completed', { transferId: `t${index}`, direction: 'download', key: item.key })), localRoot: '/target' });
    const failure = error('PEER_REVEAL_FAILED', 'original reveal failure');
    handlers.transfer_start_download = () => queueSummary('completed');
    handlers.transfer_reveal_download = () => { throw failure; };
    const wrapper = await browser();
    await wrapper.findAll('[data-test=file-row]')[0].trigger('click');
    await wrapper.findAll('[data-test=file-row]')[1].trigger('click', { ctrlKey: true });
    await click(wrapper, '[data-test=download]');
    app.handleFailure.mockClear();
    await click(wrapper, '[data-test=reveal-download]');
    expect(app.handleFailure).toHaveBeenCalledWith(failure);
    expect(wrapper.find('[data-test=download-dialog]').exists()).toBe(true);
  });
  it("toasts a completed upload, refreshes the folder and drops it from the list", async () => {
    const wrapper = await browser();
    const listsBefore = nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_list').length;
    handlers.cloud_object_status = () => [transfer('completed', { direction: 'upload', key: 'a.txt' })];
    await click(wrapper, '[data-test=upload]');
    await vi.advanceTimersByTimeAsync(1500);
    await flushPromises();
    expect(app.setNotice).toHaveBeenCalledWith('cloudBrowser.uploaded', { key: 'a.txt' });
    expect(nativeInvoke.mock.calls.filter(([name]) => name === 'cloud_object_list').length).toBeGreaterThan(listsBefore);
    expect(wrapper.find('.cloud-transfers').exists()).toBe(false);
  });
  it("shows an accessible list skeleton until the listing arrives", async () => {
    const late = deferred();
    handlers.cloud_object_list = () => late.promise;
    const wrapper = await browser();
    expect(wrapper.find('.file-view--placeholder').exists()).toBe(true);
    expect(wrapper.find('.browser-skeleton--list').attributes('role')).toBe('status');
    expect(wrapper.find('.browser-skeleton').attributes('aria-busy')).toBe('true');
    expect(wrapper.find('.browser-skeleton').attributes('aria-label')).toBe(t('common.loading'));
    expect(wrapper.find('.browser-skeleton .el-skeleton__item').exists()).toBe(true);
    expect(wrapper.find('.browser-skeleton').text()).toBe('');
    expect(wrapper.find('.file-table').exists()).toBe(false);
    late.resolve(page([file]));
    await flushPromises();
    expect(wrapper.find('.file-view--placeholder').exists()).toBe(false);
    expect(wrapper.findAll('[data-test=file-row]')).toHaveLength(1);
  });
  it("moves the selection with arrow keys and goes up one level like the LAN browser", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix
      ? page([{ key: "docs/a.txt", size: 1 }])
      : page([{ key: "docs/a.txt", size: 1 }, { key: "b.txt", size: 2 }]);
    const wrapper = await browser();
    const view = wrapper.find(".file-view");
    await view.trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.find(".selection-summary").text()).toContain("docs");
    await view.trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.find(".selection-summary").text()).toContain("b.txt");
    await view.trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.find(".selection-summary").text()).toContain("docs");
    await wrapper.find("[data-test=folder-row]").trigger("dblclick");
    await flushPromises();
    expect(wrapper.find(".breadcrumb.is-current").text()).toBe("docs");
    await view.trigger("keydown", { key: "ArrowUp", altKey: true });
    await flushPromises();
    expect(wrapper.find(".breadcrumb.is-current").text()).toBe("main");
  });
  it("opens a folder from the column view with a single click", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix
      ? page([{ key: "docs/a.txt", size: 1 }])
      : page([{ key: "docs/a.txt", size: 1 }, file]);
    const wrapper = await browser();
    await click(wrapper, '[aria-label="Columns"]');
    await click(wrapper, ".column-row");
    expect(wrapper.find(".breadcrumb.is-current").text()).toBe("docs");
    expect(wrapper.findAll(".column")).toHaveLength(2);
  });
  it("hides the selection bar in gallery view without a selection and after clearing a multi selection", async () => {
    handlers.cloud_object_list = () => page([file, { key: "b.txt", size: 2 }]);
    handlers.cloud_object_preview = () => ({ fileName: "a.txt", mime: "text/plain", encoding: "utf8", data: "x", size: 1 });
    const wrapper = await browser();
    await click(wrapper, '[aria-label="Gallery"]');
    expect(wrapper.find(".selection-bar").exists()).toBe(false);
    const thumbs = wrapper.findAll(".gallery-thumb");
    await thumbs[0].trigger("click");
    await thumbs[1].trigger("click", { ctrlKey: true });
    expect(wrapper.find(".selection-bar--multi").exists()).toBe(true);
    await textButton(wrapper, t("peer.clearSelection")).trigger("click");
    expect(wrapper.find(".selection-bar").exists()).toBe(false);
  });
  it("offers an explicit open action for a selected folder", async () => {
    handlers.cloud_object_list = ({ prefix }) => prefix
      ? page([{ key: "docs/a.txt", size: 1 }])
      : page([{ key: "docs/a.txt", size: 1 }, file]);
    const wrapper = await browser();
    await click(wrapper, "[data-test=folder-row]");
    await textButton(wrapper, t("peer.open")).trigger("click");
    await flushPromises();
    expect(wrapper.find(".breadcrumb.is-current").text()).toBe("docs");
  });
  it("shows the shortcuts panel backed by the real behaviours", async () => {
    const wrapper = await browser();
    expect(document.querySelector(".shortcuts-panel")).toBeNull();
    await click(wrapper, '[aria-label="' + t("peer.shortcuts") + '"]');
    const panel = document.querySelector(".shortcuts-panel");
    expect(panel).not.toBeNull();
    expect(panel.querySelectorAll(".shortcuts-panel__row")).toHaveLength(8);
    expect(wrapper.find(".shortcuts-trigger").attributes("aria-expanded")).toBe("true");
    await wrapper.find(".shortcuts-trigger").trigger("click");
    expect(document.querySelector(".shortcuts-panel")).toBeNull();
  });
  it("has identical real bilingual keys and renders both locales without raw keys", async () => {
    const keys = Object.keys(en).filter((key) => key.startsWith("cloudBrowser.")); expect(keys.sort()).toEqual(Object.keys(zh).filter((key) => key.startsWith("cloudBrowser.")).sort()); for (const language of ["en", "zh-CN"]) { setLocale(language); const wrapper = await browser(); expect(wrapper.find('.sidebar-identity strong').text()).toBe('Main'); expect(wrapper.text()).not.toContain("cloudBrowser."); for (const key of keys) expect(t(key)).not.toBe(key); wrapper.unmount(); }
  });
});
