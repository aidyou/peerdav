import { describe, it, expect, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { readFileSync } from 'node:fs';
import DirectoryTreePicker from '../components/DirectoryTreePicker.vue';

const mountTree = (loadChildren) => mount(DirectoryTreePicker, { props: { open: true, loadChildren }, global: { stubs: { FileGlyph: true } } });
describe('shared directory tree', () => {
  it('expands in place, preserves siblings and caches collapsed branches', async () => {
    const load = vi.fn(async id => id === '' ? [{ id: 'a', name: 'A' }, { id: 'b', name: 'B' }] : [{ id: 'a/child', name: 'Child' }]);
    const updated = vi.fn();
    const wrapper = mount(DirectoryTreePicker, { props: { open: true, loadChildren: load, 'onUpdate:modelValue': updated }, global: { stubs: { FileGlyph: true } } }); await flushPromises();
    expect(wrapper.findAll('[role=treeitem]')).toHaveLength(3);
    await wrapper.find('[data-directory=a]').trigger('click'); await flushPromises();
    expect(wrapper.findAll('[role=treeitem]')).toHaveLength(4);
    expect(wrapper.find('[data-directory=b]').exists()).toBe(true);
    expect(updated).toHaveBeenLastCalledWith('a');
    const toggle = wrapper.find('[data-directory=a]').element.parentElement.querySelector('.directory-tree__toggle');
    toggle.click(); await flushPromises();
    expect(wrapper.find('[data-directory="a/child"]').exists()).toBe(false);
    toggle.click(); await flushPromises();
    expect(load).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });
  it('ignores a late result after closing and reopening', async () => {
    let resolve;
    const load = vi.fn().mockImplementationOnce(() => new Promise(done => { resolve = done; })).mockResolvedValue([]);
    const wrapper = mountTree(load);
    await wrapper.setProps({ open: false }); await wrapper.setProps({ open: true }); await flushPromises();
    resolve([{ id: 'stale', name: 'Stale' }]); await flushPromises();
    expect(wrapper.find('[data-directory=stale]').exists()).toBe(false);
    wrapper.unmount();
  });
  it('provides one global 10px footer contract without relying on adjacent margins', () => {
    const app = readFileSync('src/App.vue', 'utf8');
    expect(app).toMatch(/:root \.el-dialog__footer\s*\{[^}]*gap: 10px/s);
    expect(app).toMatch(/:root \.el-dialog__footer \.el-button \+ \.el-button\s*\{\s*margin-left: 0/s);
    for (const file of ['CloudBrowser.vue', 'PeerBrowser.vue']) {
      const source = readFileSync(`src/components/${file}`, 'utf8');
      expect(source).toContain('<DirectoryTreePicker');
      const bar = source.slice(source.indexOf('class="selection-bar'));
      const move = bar.indexOf(file === 'CloudBrowser.vue' ? 'data-test="local-move"' : 'data-test="move"');
      const remove = bar.indexOf(file === 'CloudBrowser.vue' ? 'data-test="local-bar-delete"' : 'data-test="delete-file"');
      expect(move).toBeGreaterThan(0); expect(move).toBeLessThan(remove);
    }
  });
});
