import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount, flushPromises, DOMWrapper } from '@vue/test-utils';
import { defineComponent, h, nextTick } from 'vue';
import { ElDialog } from 'element-plus';
import UploadBatchDialog from '../components/UploadBatchDialog.vue';
import TransferQueueDialog from '../components/TransferQueueDialog.vue';
import { useUploadBatch } from '../composables/useUploadBatch';
import { useDownloadQueue } from '../composables/useDownloadQueue';
import { locale, t } from '../lib/i18n';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('../lib/invoke', () => ({ invoke, errorStrategy: () => 'report', normalizeAppError: e => e }));
const wrappers = [];
const initialLocale = locale.value;
afterEach(() => { wrappers.splice(0).forEach(w => w.unmount()); document.body.innerHTML = ''; locale.value = initialLocale; vi.clearAllMocks(); });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const state = (phase, conflict = null, id = 'queue-1') => ({ phase, conflict, batchId: id, queueId: id, completedFiles: 0, completedBytes: 0, skippedFiles: 0 });
const conflict = id => ({ conflictId: id, relativePath: `${id}.txt`, key: `${id}.txt`, etag: 'safe' });
const settle = async () => { await flushPromises(); await nextTick(); };
const click = async selector => { const node = document.querySelector(selector); expect(node).not.toBeNull(); await new DOMWrapper(node).trigger('click'); await settle(); };

function setup(direction) {
  let current = state('conflict', conflict('first'));
  let response = null;
  const resolve = vi.fn(async () => { if (response) await response.promise; });
  const cancel = vi.fn(async () => { current = state('canceled'); });
  let queue;
  const failure = vi.fn();
  if (direction === 'upload') {
    const adapter = { identity: () => ({}), select: async () => state('selected'), start: async () => current,
      status: async () => current, page: async () => ({ entries: [] }), resolve, cancel, release: async () => {},
      overwriteAllowed: () => true, isCursorReset: () => false, isCancellation: () => false };
    queue = useUploadBatch({ adapter, captureScope: () => ({}), scopeValid: () => true, canSelect: () => true, onFailure: failure, onCompleted: vi.fn() });
  } else {
    invoke.mockImplementation(async (command, args) => {
      if (command === 'transfer_resolve_download') return resolve(args);
      if (command === 'transfer_cancel_download') return cancel(args);
      if (command === 'transfer_page_download') return { items: [], nextOffset: null };
      if (command === 'transfer_release_download') return;
      return current;
    });
    queue = useDownloadQueue({ captureScope: () => ({}), scopeValid: () => true, backend: () => ({}), onFailure: failure });
  }
  const upload = direction === 'upload';
  const apply = upload ? queue.uploadApplyRemaining : queue.downloadApplyRemaining;
  const props = () => upload ? {
    open: queue.uploadDialogOpen.value, summary: queue.uploadSummary.value, running: queue.uploadRunning.value,
    resolving: queue.uploadResolving.value, overwriteAllowed: true, applyRemaining: apply.value,
    onResolve: queue.resolveUploadConflict, onCancel: queue.cancelUpload,
    'onUpdate:applyRemaining': value => { apply.value = value; },
  } : {
    open: queue.downloadDialogOpen.value, queueState: queue.downloadState.value, running: queue.downloadRunning.value,
    canceling: queue.downloadCanceling.value, resolving: queue.downloadResolving.value, applyRemaining: apply.value,
    onResolve: queue.resolveDownloadConflict, onCancel: queue.cancelDownloadQueue,
    'onUpdate:applyRemaining': value => { apply.value = value; },
  };
  const Parent = defineComponent({ setup: () => () => h(upload ? UploadBatchDialog : TransferQueueDialog, props()) });
  const wrapper = mount(Parent, { attachTo: document.body }); wrappers.push(wrapper);
  return { wrapper, apply, resolve, cancel, failure,
    select: upload ? queue.selectUpload : () => queue.selectDownload([]),
    poll: upload ? queue.pollUpload : queue.pollDownload,
    close: upload ? queue.closeUpload : queue.closeDownloadQueue,
    setState: value => { current = value; }, hold: () => { response = deferred(); return response; },
  };
}
const modal = direction => document.querySelector(`[data-test=${direction}-conflict-dialog]`);
const checkbox = direction => document.querySelector(`[data-test=${direction}-apply-remaining]`);

describe.each(['upload', 'download'])('real Element Plus %s conflict lifecycle', direction => {
  it.each([['zh-CN', '文件已存在'], ['en', 'File already exists']])('uses direction-neutral %s title and body', async (language, message) => {
    locale.value = language;
    const s = setup(direction); await s.select(); await settle();
    const dialog = s.wrapper.findAllComponents(ElDialog).find(w => w.props('appendToBody'));
    expect(dialog.props('title')).toBe(message);
    expect(modal(direction).querySelector('.el-dialog__title').textContent).toBe(message);
    // 正文不复述标题，只说明后果；路径单独成块展示。
    expect(modal(direction).querySelector(`[data-test=${direction}-conflict] p`).textContent)
      .toBe(t(direction === 'upload' ? 'cloudBrowser.conflictBodyUpload' : 'cloudBrowser.conflictBodyDownload'));
    expect(modal(direction).querySelector('.conflict-path__value').textContent).toBe('first.txt');
  });

  it.each(['"bae60998ffe4923b131e3d6e4c19993e"', 'W/"revision"'])('labels the remote path and strips quotes from ETag %s', async etag => {
    const s = setup(direction);
    s.setState(state('conflict', { ...conflict('first'), etag }));
    await s.select(); await settle();
    const details = [...modal(direction).querySelectorAll('.conflict-details p')].map(p => p.textContent);
    expect(details).toContain(`${t('cloudBrowser.conflictRemotePath')}first.txt`);
    expect(details).toContain(`ETag: ${etag.replaceAll('"', '')}`);
    expect(details.join('')).not.toContain('"');
    await click(`[data-test=${direction}-overwrite]`);
    expect(s.resolve.mock.calls[0][0]).toMatchObject({ conflictId: 'first', decision: 'overwrite', applyToRemaining: false });
  });

  it.each(['overwrite', 'skip'])('asks in a distinct modal again after unchecked %s, suppresses resolved stale IDs, and resets a new queue', async decision => {
    const s = setup(direction);
    await s.select(); await settle();
    const first = modal(direction);
    expect(first.querySelector('[role=dialog]') || first.closest('[role=dialog]')).not.toBeNull();
    expect(first.closest('.el-overlay').style.display).not.toBe('none');
    expect(document.querySelectorAll('[role=dialog]')).toHaveLength(2);
    expect(checkbox(direction).checked).toBe(false);
    const gate = s.hold();
    await click(`[data-test=${direction}-${decision}]`);
    expect(s.resolve).toHaveBeenCalledTimes(1);
    await click(`[data-test=${direction}-skip]`);
    expect(s.resolve).toHaveBeenCalledTimes(1);
    gate.resolve(); await settle(); // Status deliberately still returns the resolved ID.
    expect(modal(direction)).toBeNull();
    await s.poll(); await settle();
    expect(modal(direction)).toBeNull();
    expect(s.resolve.mock.calls[0][0]).toMatchObject({ conflictId: 'first', decision, applyToRemaining: false });
    s.setState(state('conflict', conflict('second')));
    await s.poll(); await settle();
    expect(modal(direction)).not.toBeNull();
    expect(modal(direction)).not.toBe(first);
    expect(modal(direction).closest('.el-overlay').style.display).not.toBe('none');
    expect(modal(direction).textContent).toContain('second.txt');
    expect(checkbox(direction).checked).toBe(false);
    expect(s.resolve).toHaveBeenCalledTimes(1);
    s.setState(state('completed'));
    await click(`[data-test=${direction}-${decision}]`);
    expect(s.resolve.mock.calls[1][0]).toMatchObject({ conflictId: 'second', decision, applyToRemaining: false });
    expect(modal(direction)).toBeNull();
    s.apply.value = true;
    await s.close();
    s.setState(state('conflict', conflict('first'), 'queue-2'));
    await s.select(); await settle();
    expect(modal(direction)).not.toBeNull();
    expect(checkbox(direction).checked).toBe(false);
    expect(s.failure).not.toHaveBeenCalled();
  });

  it('sends queue caching only when checked and resets the next exceptional conflict', async () => {
    const s = setup(direction); await s.select(); await settle();
    await new DOMWrapper(checkbox(direction)).setValue(true); await settle();
    s.setState(state('conflict', conflict('second')));
    await click(`[data-test=${direction}-overwrite]`);
    expect(s.resolve.mock.calls[0][0]).toMatchObject({ applyToRemaining: true });
    expect(modal(direction).textContent).toContain('second.txt');
    expect(checkbox(direction).checked).toBe(false);
  });

  it('cannot silently escape or click outside; dedicated cancel joins cancellation', async () => {
    const s = setup(direction); await s.select(); await settle();
    const dialogs = s.wrapper.findAllComponents(ElDialog);
    const dialog = dialogs.find(w => w.props('appendToBody'));
    expect(dialog.props()).toMatchObject({ closeOnClickModal: false, closeOnPressEscape: false, showClose: false, appendToBody: true });
    await new DOMWrapper(document).trigger('keydown', { key: 'Escape', code: 'Escape' });
    await new DOMWrapper(modal(direction).querySelector('.el-overlay-dialog') || modal(direction).closest('.el-overlay-dialog')).trigger('click');
    await settle(); expect(modal(direction)).not.toBeNull();
    expect(s.cancel).not.toHaveBeenCalled(); expect(s.resolve).not.toHaveBeenCalled();
    await click(`[data-test=${direction}-conflict-cancel]`);
    expect(s.cancel).toHaveBeenCalledTimes(1); expect(s.resolve).not.toHaveBeenCalled();
    expect(modal(direction)).toBeNull();
  });
});
