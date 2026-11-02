import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount, DOMWrapper } from '@vue/test-utils';
const click = async (selector) => { await new DOMWrapper(document.querySelector(selector)).trigger('click'); };
import { nextTick, defineComponent, h } from 'vue';
import { ElButton, ElDialog } from 'element-plus';
import TransferConflictControls from '../components/TransferConflictControls.vue';
import TransferQueueDialog from '../components/TransferQueueDialog.vue';
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
    expect(document.querySelector('.queue-note').previousElementSibling.classList.contains('download-queue')).toBe(true);
    await click('[data-test=cancel-download]'); await nextTick();
    expect(w.events.onCancel).toHaveBeenCalledTimes(1);
    await w.setProps({ canceling: true }); await click('[data-test=cancel-download]');
    expect(w.events.onCancel).toHaveBeenCalledTimes(1);
    expect(w.events.onClose).not.toHaveBeenCalled();
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
