import { afterEach, describe, expect, it, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { defineComponent, h, ref, toRaw } from "vue";
import FileManagerView from "../components/FileManagerView.vue";
import { selectionForClick } from "../lib/fileManagerAdapters";

const wrappers = [];
afterEach(() => wrappers.splice(0).forEach((wrapper) => wrapper.unmount()));
const raw = [" 中文%2F/", "中文%/", "中文/", "中文%2F"];
const entriesFor = (field) => raw.map((identity, index) => ({ [field]: identity, name: identity, isDir: index === 0, size: index, modified: 123 }));
const defaults = { entryKind: (entry) => entry.isDir ? "Folder" : "File", formatSize: (entry) => String(entry.size), formatModified: (value) => String(value) };
function view(props = {}, slots = {}) {
  const wrapper = mount(FileManagerView, { props: { ...defaults, ...props }, slots });
  wrappers.push(wrapper);
  return wrapper;
}

describe("shared FileManagerView", () => {
  it.each(['icons', 'list', 'columns', 'gallery'])('offers internal %s dragging only to folders with raw identities', async mode => {
    const entries = entriesFor('path');
    const onMove = vi.fn();
    const wrapper = view({ entries, mode, moveEnabled: true, onMove, selection: [entries[1]], columns: [{ path: '', entries }] });
    const selector = { icons: '.file-tile', list: 'tbody tr', columns: '.column-row', gallery: '.gallery-thumb' }[mode];
    const rows = wrapper.findAll(selector);
    const dataTransfer = { setData: vi.fn(), effectAllowed: '', dropEffect: '' };
    await rows[1].trigger('dragstart', { dataTransfer });
    await rows[2].trigger('drop', { dataTransfer });
    expect(onMove).not.toHaveBeenCalled();
    await rows[0].trigger('drop', { dataTransfer });
    expect(toRaw(onMove.mock.calls[0][0][0])).toBe(entries[1]);
    expect(toRaw(onMove.mock.calls[0][1])).toBe(entries[0]);
    await rows[1].trigger('dragend');
    await rows[0].trigger('drop', { dataTransfer });
    expect(onMove.mock.calls).toHaveLength(1);
  });
  for (const identityField of ["key", "path"]) {
    for (const mode of ["icons", "list", "columns", "gallery"]) {
      it(`${identityField} provider renders real ${mode} DOM with raw identities and events`, async () => {
        const entries = entriesFor(identityField);
        const column = { [identityField]: " 根%2F/", entries, complete: true };
        const onSelect = vi.fn(); const onColumnSelect = vi.fn(); const onActivate = vi.fn(); const onPreview = vi.fn();
        const wrapper = view({ entries, identityField, mode, selection: [entries[1]], columns: [column], columnIdentityField: identityField, onSelect, onColumnSelect, onActivate, onPreview }, { "gallery-preview": "<img alt='preview' src='preview.png' />" });
        const selector = { icons: ".file-tile", list: ".file-table tbody tr", columns: ".column-row", gallery: ".gallery-thumb" }[mode];
        const rows = wrapper.findAll(selector);
        expect(rows).toHaveLength(4);
        expect(rows.map((row) => row.attributes(`data-${identityField}`))).toEqual(raw);
        expect(rows[1].classes()).toContain("is-selected");
        await rows[1].trigger("click", { ctrlKey: true, metaKey: true, shiftKey: true });
        const event = (mode === "columns" ? onColumnSelect : onSelect).mock.calls[0];
        expect(toRaw(event[0])).toBe(entries[1]);
        expect(event[1]).toMatchObject({ ctrlKey: true, metaKey: true, shiftKey: true });
        if (mode === "columns") { expect(toRaw(event[2])).toBe(column); expect(event[3]).toBe(0); }
        await rows[0].trigger("dblclick");
        expect(toRaw((mode === "columns" ? onPreview : onActivate).mock.calls[0][0])).toBe(entries[0]);
        if (mode === "list") {
          await rows[2].trigger("keydown", { key: "Enter" });
          expect(toRaw(onActivate.mock.calls.at(-1)[0])).toBe(entries[2]);
          expect(wrapper.findAll("th")).toHaveLength(3);
          await wrapper.setProps({ timeColumn: "modified" });
          expect(wrapper.findAll("th")).toHaveLength(4);
        }
        if (mode === "gallery") expect(wrapper.find(".gallery-stage img").attributes("alt")).toBe("preview");
      });
    }
  }

  it.each(['icons', 'list', 'columns', 'gallery'])('keeps %s sync actions separate from navigation and avoids nested buttons', async mode => {
    const entries = entriesFor('path');
    const onStatusAction = vi.fn(); const onActivate = vi.fn(); const onSelect = vi.fn(); const onColumnSelect = vi.fn(); const onPreview = vi.fn();
    const wrapper = view({ entries, mode, columns: [{ path: '', entries, complete: true }], entryStatus: entry => entry.path === entries[0].path ? 'Not synced · Sync' : '', onStatusAction, onActivate, onSelect, onColumnSelect, onPreview });
    const status = wrapper.find('.entry-status');
    expect(status.element.tagName).toBe('BUTTON');
    expect(status.element.parentElement.closest('button')).toBeNull();
    await status.trigger('click'); await status.trigger('dblclick');
    expect(toRaw(onStatusAction.mock.calls[0][0])).toBe(entries[0]);
    expect(onSelect).not.toHaveBeenCalled(); expect(onColumnSelect).not.toHaveBeenCalled();
    expect(onActivate).not.toHaveBeenCalled(); expect(onPreview).not.toHaveBeenCalled();
  });

  it("keeps the provider's selection as the sole state through plain, additive and range clicks", async () => {
    const entries = entriesFor("key");
    const Host = defineComponent({ setup() {
      const selection = ref([]);
      let anchor = null;
      return () => h(FileManagerView, { ...defaults, entries, mode: "list", identityField: "key", selection: selection.value,
        onSelect(entry, event) {
          const result = selectionForClick({ entries, selection: selection.value, anchor, entry, identityField: "key", additive: event.ctrlKey || event.metaKey, ranged: event.shiftKey });
          selection.value = result?.selection ?? [entry];
          anchor = result ? result.anchor : entry.key;
        },
      });
    } });
    const wrapper = mount(Host); wrappers.push(wrapper);
    const rows = wrapper.findAll("tbody tr");
    await rows[0].trigger("click");
    await rows[2].trigger("click", { ctrlKey: true });
    expect(wrapper.findAll(".is-selected").map(row => row.attributes("data-key"))).toEqual([raw[0], raw[2]]);
    await rows[0].trigger("click", { metaKey: true });
    expect(wrapper.findAll(".is-selected")).toHaveLength(1);
    await rows[3].trigger("click", { shiftKey: true });
    expect(wrapper.findAll(".is-selected").map(row => row.attributes("data-key"))).toEqual(raw.slice(2));
  });

  it("distinguishes loaded-empty, loading, failure, and partial listings", async () => {
    const wrapper = view({ emptyLabel: "Empty" }, { partial: "<p data-test='partial'>Partial</p>" });
    expect(wrapper.find(".file-empty").text()).toBe("Empty");
    await wrapper.setProps({ loading: true });
    expect(wrapper.find("[aria-busy=true]").exists()).toBe(true);
    expect(wrapper.find(".file-empty").exists()).toBe(false);
    await wrapper.setProps({ loading: false, error: { code: "FAILED", message: "Original" } });
    expect(wrapper.find(".file-empty").exists()).toBe(false);
    await wrapper.setProps({ error: null, complete: false, partial: true, entries: entriesFor("path") });
    expect(wrapper.findAll(".file-tile")).toHaveLength(4);
    expect(wrapper.find("[data-test=partial]").text()).toBe("Partial");
    await wrapper.setProps({ mode: "columns", entries: [], columns: [{ path: "原%/", entries: [], failed: true, complete: false }] });
    expect(wrapper.find(".column-empty").exists()).toBe(false);
    await wrapper.setProps({ columns: [{ path: "原%/", entries: [], loading: true }], columnLoading: true });
    expect(wrapper.find(".column-view").attributes("aria-busy")).toBe("true");
    expect(wrapper.find(".column-loading").exists()).toBe(true);
  });
});
