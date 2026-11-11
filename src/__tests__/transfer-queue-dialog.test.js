import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount, DOMWrapper } from '@vue/test-utils';
const click = async (selector) => { await new DOMWrapper(document.querySelector(selector)).trigger('click'); };
import { nextTick, defineComponent, h } from 'vue';
import { ElButton, ElDialog } from 'element-plus';
import TransferConflictControls from '../components/TransferConflictControls.vue';
import TransferQueueDialog from '../components/TransferQueueDialog.vue';
import UploadBatchDialog from '../components/UploadBatchDialog.vue';
import OperationErrorDialog from '../components/OperationErrorDialog.vue';
import { setLocale } from '../lib/i18n';
const wrappers = [];
const render = (component, props) => {
  // Assert through actual parent listeners: this environment's Vue runtime does
  // not report component events to VTU's devtools-backed emitted() recorder.
  const events = { onCancel: vi.fn(), onClose: vi.fn(), onRetry: vi.fn(), onReveal: vi.fn() };
  const w = mount(component, { props: { ...events, ...props }, attachTo: document.body });
  w.events = events; wrappers.push(w); return w;
};
afterEach(() => { wrappers.splice(0).forEach(w => w.unmount()); document.body.innerHTML = ''; });
describe('mounted transfer dialogs', () => {
  it.each(['upload', 'download'])('uses the same explicit conflict controls and dedicated cancellation for %s with actual Element Plus', async (direction) => {
    const resolve = vi.fn(); const cancel = vi.fn(); const apply = vi.fn();
    render(TransferConflictControls, { direction, conflict: { conflictId: 'c', relativePath: 'a.txt' }, overwriteAllowed: true, onResolve: resolve, onCancel: cancel, 'onUpdate:applyRemaining': apply });
    await nextTick();
    expect(document.querySelector(`[data-test=${direction}-apply-remaining]`).checked).toBe(false);
    await new DOMWrapper(document.querySelector(`[data-test=${direction}-apply-remaining]`)).setValue(true);
    expect(apply).toHaveBeenCalledWith(true);
    await click(`[data-test=${direction}-overwrite]`); expect(resolve).toHaveBeenCalledWith('overwrite');
    await click(`[data-test=${direction}-skip]`); expect(resolve).toHaveBeenCalledWith('skip');
    await click(`[data-test=${direction}-conflict-cancel]`); expect(cancel).toHaveBeenCalledTimes(1);
    expect(resolve).not.toHaveBeenCalledWith('cancel');
  });
  it('proves the real dialog button emit chain independently', async () => {
    const events = [];
    const Probe = defineComponent({ emits: ['retry'], setup(_, { emit }) { return () => h(ElDialog, { modelValue: true }, { footer: () => h(ElButton, { 'data-test': 'probe', onClick: () => emit('retry') }, () => 'Retry') }); } });
    render(Probe, { onRetry: () => events.push('retry') });
    await nextTick(); await click('[data-test=probe]');
    expect(events).toEqual(['retry']);
  });
  it('shows unknown progress honestly, collapses errors, and emits cancellation only once disabled', async () => {
    setLocale('en');
    const w = render(TransferQueueDialog, { open: true, running: true, items: [{ relativePath: 'folder/a.txt', status: 'error', error: 'original failure' }], summary: '0 / 1', note: 'Queue note' });
    await nextTick();
    expect(document.querySelector('[role=dialog]')).not.toBeNull();
    expect(document.querySelector('[role=progressbar]')).toBeNull();
    expect(document.querySelector('details').open).toBe(false);
    expect(document.querySelector('.queue-header__help')).not.toBeNull();
    await click('[data-test=cancel-download]'); await nextTick();
    expect(w.events.onCancel).toHaveBeenCalledTimes(1);
    await w.setProps({ canceling: true }); await click('[data-test=cancel-download]');
    expect(w.events.onCancel).toHaveBeenCalledTimes(1);
    expect(w.events.onClose).not.toHaveBeenCalled();
  });
  it('renders a fixed header, highlights the active row with progress, and puts conditional stats in the footer', async () => {
    setLocale('en');
    const items = [
      { relativePath: 'docs/a.txt', size: 100, status: 'downloading' },
      { relativePath: 'docs/b.txt', size: 2048, status: 'done' },
      { relativePath: 'docs/c.txt', size: 10, status: 'skipped' },
    ];
    const w = render(TransferQueueDialog, { open: true, items, queueState: { queueId: 'q', completedFiles: 1, totalFiles: 3, skippedFiles: 2, currentRelativePath: 'docs/a.txt', currentTransferred: 50 } });
    await nextTick();
    const head = document.querySelector('.queue-table__head').textContent;
    expect(head).toContain('Path');
    expect(head).toContain('Size');
    expect(head).toContain('Status');
    const rows = w.findAll('.download-queue li');
    expect(rows[0].text()).toContain('docs/a.txt');
    expect(rows[0].text()).toContain('100');
    expect(rows[0].classes()).toContain('is-active');
    expect(rows[0].attributes('data-progress')).toBe('50');
    expect(rows[1].classes()).not.toContain('is-active');
    expect(rows[2].text()).toContain('Skipped');
    expect(document.querySelector('.dialog-footer').textContent).toContain('Processed: 3/3');
    expect(document.querySelector('.dialog-footer').textContent).toContain('Skipped: 2');
  });

  it('hides processed and skipped stats until one side has data', async () => {
    setLocale('en');
    const w = render(TransferQueueDialog, { open: true, items: [], queueState: { queueId: 'q', completedFiles: 0, totalFiles: 0, skippedFiles: 0 } });
    await nextTick();
    expect(document.querySelector('.dialog-footer').textContent).not.toContain('Processed');
    expect(document.querySelector('.dialog-footer').textContent).not.toContain('Skipped');
  });

  it('gives the upload dialog the same fixed header, active row and conditional footer stats', async () => {
    setLocale('en');
    const w = render(UploadBatchDialog, {
      open: true, running: true,
      summary: { batchId: 'b', phase: 'uploading', completedFiles: 2, completedBytes: 2048, totalFiles: 3, skippedFiles: 1, currentRelativePath: 'docs/a.txt' },
      page: { directoryToken: 'root', relativePath: 'docs', entries: [
        { name: 'a.txt', relativePath: 'docs/a.txt', kind: 'file', size: 1024 },
        { name: 'sub', relativePath: 'docs/sub', kind: 'directory', directoryToken: 'child' },
        { name: 'link', relativePath: 'docs/link', kind: 'symlink' },
      ] },
    });
    await nextTick();
    const head = document.querySelector('.queue-table__head').textContent;
    expect(head).toContain('Path');
    expect(head).toContain('Size');
    expect(head).toContain('Status');
    const rows = w.findAll('.upload-queue li');
    expect(rows).toHaveLength(3);
    expect(rows[0].classes()).toContain('is-active');
    expect(rows[0].attributes('data-status')).toBe('active');
    expect(rows[0].text()).toContain('1.0 KB');
    expect(rows[1].attributes('data-status')).toBe('unscanned');
    expect(rows[1].text()).not.toContain('Open directory');
    expect(rows[1].text()).toContain('—');
    expect(rows[2].text()).toContain('Symbolic link');
    expect(document.querySelector('.upload-dir__state.is-active .upload-dir__spinner')).not.toBeNull();
    expect(document.querySelector('.queue-header__help')).not.toBeNull();
    const footer = document.querySelector('.dialog-footer').textContent;
    expect(footer).toContain('Processed: 3/3');
    expect(footer).toContain('Uploaded: 2.0 KB');
    expect(footer).toContain('Skipped: 1');
    expect(document.querySelector('.upload-queue').textContent).not.toContain('Not tracked');
  });

  it('shows streamed bytes, proportional row fill and confirmed terminal entry states', async () => {
    setLocale('en');
    const w = render(UploadBatchDialog, { open: true, running: true, summary: { phase: 'running', currentRelativePath: 'a.txt' }, page: { entries: [
      { name: 'a.txt', relativePath: 'a.txt', kind: 'file', size: 2048, transferred: 512, phase: 'active' },
      { name: 'done.txt', relativePath: 'done.txt', kind: 'file', size: 10, transferred: 10, phase: 'completed' },
      { name: 'skip.txt', relativePath: 'skip.txt', kind: 'file', size: 10, phase: 'skipped' },
    ] } });
    await nextTick();
    const rows = w.findAll('.upload-queue li');
    expect(rows[0].text()).toContain('512 B/2.0 KB');
    expect(rows[0].element.style.getPropertyValue('--upload-progress')).toBe('25%');
    expect(rows[1].find('.queue-cell--status').text()).toBe('Uploaded');
    expect(rows[2].find('.queue-cell--status').text()).toBe('Skipped');
    await w.setProps({ running: false, summary: { phase: 'completed' }, page: { entries: [{ name: 'a.txt', relativePath: 'a.txt', kind: 'file', size: 2048, transferred: 2048, phase: 'completed' }] } });
    expect(w.find('.queue-cell--status').text()).toBe('Uploaded');
    expect(w.find('.is-active').exists()).toBe(false);
  });

  it('marks the upload folder row with a spinner, success check or error cross', async () => {
    setLocale('en');
    const base = { batchId: 'b', completedFiles: 0, completedBytes: 0, totalFiles: 0, skippedFiles: 0 };
    const rest = { open: true, running: true, page: { relativePath: 'docs/a/very/long/path', entries: [] } };
    const w = render(UploadBatchDialog, { ...rest, summary: { ...base, phase: 'uploading' } });
    await nextTick();
    expect(document.querySelector('.upload-dir__path').textContent).toContain('docs/a/very/long/path');
    const spinner = document.querySelector('.upload-dir__state.is-active .upload-dir__spinner');
    expect(spinner).not.toBeNull();
    expect(spinner.tagName.toLowerCase()).toBe('span');
    expect(spinner.querySelector('svg circle')).not.toBeNull();
    await w.setProps({ summary: { ...base, phase: 'completed' } });
    expect(document.querySelector('.upload-dir__state.is-done svg')).not.toBeNull();
    await w.setProps({ summary: { ...base, phase: 'failed' } });
    expect(document.querySelector('.upload-dir__state.is-error svg')).not.toBeNull();
    await w.setProps({ summary: { ...base, phase: 'cancelled' } });
    expect(document.querySelector('.upload-dir__state')).toBeNull();
  });

  it.each(['upload', 'download'])('opens the %s queue help on click instead of hover', async (direction) => {
    setLocale('en');
    const component = direction === 'upload' ? UploadBatchDialog : TransferQueueDialog;
    const props = direction === 'upload'
      ? { open: true, running: true, summary: { batchId: 'b', phase: 'uploading', completedFiles: 1, completedBytes: 10, totalFiles: 1, skippedFiles: 0 }, page: { entries: [] } }
      : { open: true, items: [], queueState: { queueId: 'q', completedFiles: 0, totalFiles: 0, skippedFiles: 0 }, note: 'Queue note' };
    render(component, props);
    await nextTick();
    const helpNode = () => document.querySelector('.el-popover .queue-help');
    const hidden = () => { const popper = helpNode().closest('.el-popper'); return popper.style.display === 'none' || popper.hidden || popper.getAttribute('aria-hidden') === 'true'; };
    expect(hidden()).toBe(true);
    await click('.queue-header__help');
    await new Promise((resolve) => setTimeout(resolve, 20)); await nextTick();
    expect(hidden()).toBe(false);
    expect(helpNode().textContent).toContain(direction === 'upload' ? 'direct children' : 'downloaded one by one');
  });

  it('hides upload footer stats until files or skips exist', async () => {
    setLocale('en');
    render(UploadBatchDialog, { open: true, summary: { batchId: 'b', phase: 'selected', completedFiles: 0, completedBytes: 0, skippedFiles: 0 }, page: { entries: [] } });
    await nextTick();
    const footer = document.querySelector('.dialog-footer').textContent;
    expect(footer).not.toContain('uploaded');
    expect(footer).not.toContain('Skipped');
  });

  it('offers only supported retry, reveal and close actions after the queue stops', async () => {
    const item = { transferId: 'target', relativePath: 'a.txt', status: 'error', retryable: true };
    const w = render(TransferQueueDialog, { open: true, root: '/target', items: [item] }); await nextTick();
    await click('[data-test=retry-download]'); await click('[data-test=reveal-download]'); await click('[data-test=close-download]');
    expect(w.events.onRetry).toHaveBeenCalledExactlyOnceWith(item); expect(w.events.onReveal).toHaveBeenCalledTimes(1); expect(w.events.onClose).toHaveBeenCalledTimes(1);
    expect(document.querySelector('[data-test=cancel-download]')).toBeNull();
  });
  it('keeps operation details folded and retries only when an operation is available', async () => {
    const w = render(OperationErrorDialog, { error: { code: 'CLOUD_TRANSPORT', message: 'Original reason', detail: 'diagnostic' }, retryable: true }); await nextTick();
    expect(document.querySelector('details').open).toBe(false);
    await click('[data-test=retry-load]'); expect(w.events.onRetry).toHaveBeenCalledTimes(1);
    await w.setProps({ retryable: false }); expect(document.querySelector('[data-test=retry-load]')).toBeNull();
  });
});
