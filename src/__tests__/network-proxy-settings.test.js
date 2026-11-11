import { mount, flushPromises } from '@vue/test-utils';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { defineComponent, h } from 'vue';
import NetworkProxySettings from '../components/NetworkProxySettings.vue';
const { invoke, failure } = vi.hoisted(() => ({ invoke: vi.fn(), failure: vi.fn() }));
vi.mock('../lib/invoke', () => ({ invoke }));
vi.mock('../lib/appContext', () => ({ useAppContext: () => ({ handleFailure: failure }) }));
vi.mock('../lib/i18n', () => ({ t: key => key }));
const stubs = {
  'el-select': defineComponent({ props: ['modelValue'], emits: ['update:modelValue'], setup(props, { emit, slots }) { return () => h('select', { value: props.modelValue, onChange: event => emit('update:modelValue', event.target.value) }, slots.default?.()); } }),
  'el-option': defineComponent({ props: ['value', 'label'], setup(props) { return () => h('option', { value: props.value }, props.label); } }),
  'el-input': defineComponent({ props: ['modelValue', 'type'], emits: ['update:modelValue'], setup(props, { emit }) { return () => h('input', { type: props.type || 'text', value: props.modelValue, onInput: event => emit('update:modelValue', event.target.value) }); } }),
  'el-button': defineComponent({ setup(_, { slots }) { return () => h('button', { type: 'submit' }, slots.default?.()); } }),
};
describe('network proxy settings', () => {
  beforeEach(() => { vi.clearAllMocks(); });
  it('loads system default and saves HTTP credentials without persisting them in localStorage', async () => {
    invoke.mockResolvedValueOnce({ mode: 'environment' }).mockResolvedValue(undefined);
    const storage = vi.spyOn(Storage.prototype, 'setItem');
    const wrapper = mount(NetworkProxySettings, { global: { stubs } });
    await flushPromises();
    expect(wrapper.findComponent({ name: 'ElSelect' }).props('modelValue')).toBe('environment');
    wrapper.findComponent({ name: 'ElSelect' }).vm.$emit('update:modelValue', 'custom');
    await flushPromises();
    const fields = wrapper.findAll('input').filter(field => field.attributes('role') !== 'combobox');
    await fields[0].setValue('http://127.0.0.1:7890');
    await fields[1].setValue('user');
    await fields[2].setValue('private-password');
    await wrapper.find('form').trigger('submit'); await flushPromises();
    expect(invoke).toHaveBeenLastCalledWith('save_client_proxy_settings', { proxy: { mode: 'custom', url: 'http://127.0.0.1:7890', username: 'user', password: 'private-password' } });
    expect(wrapper.find('input[type="password"]').element.value).toBe('');
    expect(storage).not.toHaveBeenCalled(); storage.mockRestore(); wrapper.unmount();
  });
  it('offers only client inheritance or direct for a drive', async () => {
    invoke.mockResolvedValueOnce(false).mockResolvedValue(undefined);
    const wrapper = mount(NetworkProxySettings, { props: { connectionId: 'drive' }, global: { stubs } });
    await flushPromises();
    expect(wrapper.findComponent({ name: 'ElSelect' }).props('modelValue')).toBe('client');
    expect(wrapper.findAllComponents({ name: 'ElOption' }).map(option => option.props('value')).sort()).toEqual(['client', 'direct']);
    wrapper.findComponent({ name: 'ElSelect' }).vm.$emit('update:modelValue', 'direct');
    await flushPromises();
    await wrapper.find('form').trigger('submit'); await flushPromises();
    expect(invoke).toHaveBeenLastCalledWith('save_drive_proxy_settings', { connectionId: 'drive', direct: true });
    wrapper.unmount();
  });
});
