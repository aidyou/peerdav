<script setup>
import { onBeforeUnmount, ref } from 'vue';
import { t } from "../lib/i18n";
import FileGlyph from "./FileGlyph.vue";
import BrowserSkeleton from "./BrowserSkeleton.vue";

// Entries and columns are provider-owned objects. Identity is read verbatim:
// never decode, normalize, trim or reconstruct an object key or LAN path here.
const props = defineProps({
  moveEnabled: Boolean,
  entries: { type: Array, default: () => [] },
  mode: { type: String, default: "icons" },
  selection: { type: Array, default: () => [] },
  identityField: { type: String, default: "path" },
  columns: { type: Array, default: () => [] },
  columnIdentityField: { type: String, default: "path" },
  loading: Boolean,
  columnLoading: Boolean,
  appendLoadingColumn: Boolean,
  complete: { type: Boolean, default: true },
  error: { type: [Object, String], default: null },
  partial: Boolean,
  emptyLabel: { type: String, default: "" },
  timeColumn: { type: String, default: "" },
  timeLabel: { type: String, default: "" },
  entryKind: { type: Function, required: true },
  formatSize: { type: Function, required: true },
  formatModified: { type: Function, default: () => "" },
  entryStatus: { type: Function, default: () => "" },
});
const emit = defineEmits(["move", "select", "activate", "column-select", "preview", "column-element", "column-wheel", "status-action"]);
let dragged = null;
const dropTarget = ref(null);
function endDrag() { dragged = null; dropTarget.value = null; }
function canDrop(entry) {
  return props.moveEnabled && dragged && entry.isDir && !entry.remoteOnly && !entry.protected && !dragged.some(item => identity(item) === identity(entry));
}
function startDrag(entry, event) {
  endDrag();
  if (!props.moveEnabled || entry.remoteOnly || entry.protected) { event.preventDefault(); return; }
  dragged = props.selection.some(item => identity(item) === identity(entry)) ? props.selection.slice() : [entry];
  event.dataTransfer?.setData('application/x-peerdav-internal', 'move');
  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
}
function allowDrop(entry, event) {
  dropTarget.value = canDrop(entry) ? identity(entry) : null;
  if (canDrop(entry)) {
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  }
}
function leaveDrop(entry, event) {
  if (event.relatedTarget instanceof Node && event.currentTarget?.contains(event.relatedTarget)) return;
  if (dropTarget.value === identity(entry)) dropTarget.value = null;
}
function drop(entry, event) {
  dropTarget.value = null;
  if (!canDrop(entry)) return;
  event.preventDefault();
  const sources = dragged; endDrag();
  emit('move', sources, entry);
}
onBeforeUnmount(endDrag);
const identity = (entry) => entry[props.identityField];
const selected = (entry) => props.selection.some((item) => identity(item) === identity(entry));
const attributes = (entry) => ({
  draggable: props.moveEnabled && !entry.remoteOnly && !entry.protected,
  onDragstart: event => startDrag(entry, event),
  onDragover: event => allowDrop(entry, event),
  onDragleave: event => leaveDrop(entry, event),
  onDrop: event => drop(entry, event),
  onDragend: endDrag,
  class: { 'is-drop-target': props.moveEnabled && dropTarget.value === identity(entry) },
  [props.identityField === "key" ? "data-key" : "data-path"]: identity(entry),
  ...(entry.key !== undefined && entry.key === identity(entry) ? { 'data-key': entry.key } : {}),
});
</script>

<template>
  <div v-if="loading" class="file-view--placeholder" aria-busy="true">
    <div v-if="mode === 'columns'" class="column-view"><div class="column column-loading"><BrowserSkeleton mode="columns" /></div></div>
    <BrowserSkeleton v-else :mode="mode" />
  </div>
  <p v-else-if="!entries.length && complete && !error && mode !== 'columns'" class="file-empty">{{ emptyLabel || t('peer.emptyDir') }}</p>
  <div v-else-if="mode === 'icons'" class="icon-grid" :aria-busy="loading">
    <component :is="entryStatus(entry) ? 'div' : 'button'" v-for="entry in entries" :key="identity(entry)" v-bind="attributes(entry)" :type="entryStatus(entry) ? undefined : 'button'" :role="entryStatus(entry) ? 'group' : undefined" class="file-tile" :class="{ 'is-selected': selected(entry) }" :aria-pressed="entryStatus(entry) ? undefined : selected(entry)" :title="entry.name" @click="emit('select', entry, $event)" @dblclick="emit('activate', entry)">
      <component :is="entryStatus(entry) ? 'button' : 'span'" class="entry-main entry-main--tile" :type="entryStatus(entry) ? 'button' : undefined" :aria-pressed="entryStatus(entry) ? selected(entry) : undefined"><FileGlyph :entry="entry" size="lg" /><span class="file-tile__name" :title="entry.name">{{ entry.name }}</span></component>
      <button v-if="entryStatus(entry)" type="button" class="entry-status" @click.stop="emit('status-action', entry)" @dblclick.stop>{{ entryStatus(entry) }}</button>
    </component>
  </div>
  <div v-else-if="mode === 'list'" class="file-table-wrap">
    <table class="file-table">
      <thead><tr><th scope="col">{{ t('peer.name') }}</th><th scope="col">{{ t('peer.kind') }}</th><th scope="col">{{ t('peer.size') }}</th><th v-if="timeColumn" scope="col">{{ timeLabel || t('peer.modified') }}</th></tr></thead>
      <tbody><tr v-for="entry in entries" :key="identity(entry)" v-bind="attributes(entry)" tabindex="0" :data-test="entry.isDir ? 'folder-row' : 'file-row'" :class="{ 'is-selected': selected(entry) }" :aria-selected="selected(entry)" @click="emit('select', entry, $event)" @dblclick="emit('activate', entry)" @keydown.enter.stop="emit('activate', entry)">
        <td><span class="table-name"><FileGlyph :entry="entry" /><span>{{ entry.name }}</span><span v-if="entry.protected" class="table-protected">{{ t('cloudBrowser.protected') }}</span><button v-if="entryStatus(entry)" type="button" class="entry-status" @click.stop="emit('status-action', entry)" @dblclick.stop @keydown.enter.stop @keydown.space.stop>{{ entryStatus(entry) }}</button></span></td>
        <td>{{ entryKind(entry) }}</td><td>{{ formatSize(entry) }}</td><td v-if="timeColumn">{{ formatModified(entry[timeColumn]) }}</td>
      </tr></tbody>
    </table>
  </div>
  <div v-else-if="mode === 'columns'" :ref="element => emit('column-element', element)" class="column-view" :aria-busy="columnLoading ? 'true' : 'false'" @wheel="emit('column-wheel', $event)">
    <div v-for="(column, index) in columns" :key="column[columnIdentityField]" class="column" :class="{ 'column-loading': column.loading && !column.entries.length }">
      <BrowserSkeleton v-if="column.loading && !column.entries.length" mode="columns" />
      <component :is="entryStatus(entry) ? 'div' : 'button'" v-for="entry in column.entries" :key="identity(entry)" v-bind="attributes(entry)" :type="entryStatus(entry) ? undefined : 'button'" :role="entryStatus(entry) ? 'group' : undefined" class="column-row" :class="{ 'is-selected': selected(entry) }" :aria-pressed="entryStatus(entry) ? undefined : selected(entry)" :title="entry.name" @click="emit('column-select', entry, $event, column, index)" @dblclick="emit('preview', entry)">
        <component :is="entryStatus(entry) ? 'button' : 'span'" class="entry-main entry-main--column" :type="entryStatus(entry) ? 'button' : undefined" :aria-pressed="entryStatus(entry) ? selected(entry) : undefined"><FileGlyph :entry="entry" /><span class="column-name">{{ entry.name }}</span></component><button v-if="entryStatus(entry)" type="button" class="entry-status" @click.stop="emit('status-action', entry)" @dblclick.stop>{{ entryStatus(entry) }}</button><span v-if="entry.isDir" class="column-chevron" aria-hidden="true">›</span>
      </component>
      <p v-if="!column.entries.length && !column.loading && !column.failed && column.complete !== false" class="column-empty">{{ t('peer.emptyDir') }}</p>
    </div>
    <div v-if="appendLoadingColumn" class="column column-loading"><BrowserSkeleton mode="columns" /></div>
  </div>
  <div v-else class="gallery-view" data-testid="gallery-view">
    <div class="gallery-stage"><slot name="gallery-preview" /></div>
    <div class="gallery-strip">
      <component :is="entryStatus(entry) ? 'div' : 'button'" v-for="entry in entries" :key="identity(entry)" v-bind="attributes(entry)" :type="entryStatus(entry) ? undefined : 'button'" :role="entryStatus(entry) ? 'group' : undefined" class="gallery-thumb" :class="{ 'is-selected': selected(entry) }" :aria-pressed="entryStatus(entry) ? undefined : selected(entry)" :title="entry.name" @click="emit('select', entry, $event)" @dblclick="emit('activate', entry)"><component :is="entryStatus(entry) ? 'button' : 'span'" class="entry-main entry-main--tile" :type="entryStatus(entry) ? 'button' : undefined" :aria-pressed="entryStatus(entry) ? selected(entry) : undefined"><FileGlyph :entry="entry" /><span>{{ entry.name }}</span></component><button v-if="entryStatus(entry)" type="button" class="entry-status" @click.stop="emit('status-action', entry)" @dblclick.stop>{{ entryStatus(entry) }}</button></component>
    </div>
  </div>
  <slot name="pagination" />
  <slot v-if="partial" name="partial" />
</template>

<style scoped>
.file-tile.is-drop-target, .gallery-thumb.is-drop-target, .column-row.is-drop-target, .file-table tr.is-drop-target { outline: 2px dashed var(--selection-border); outline-offset: -2px; }
.entry-main { min-width: 0; max-width: 100%; padding: 0; border: 0; background: transparent; color: inherit; font: inherit; cursor: inherit; }
.entry-main--tile { display: flex; flex-direction: column; align-items: center; gap: 5px; width: 100%; }
.entry-main--column { display: flex; flex: 1; align-items: center; gap: 7px; text-align: left; }
.entry-main:focus-visible { outline: 2px solid var(--el-color-primary); }
.entry-status { box-sizing: border-box; min-height: 24px; border: 0; border-radius: 4px; padding: 4px 6px; background: var(--app-surface-2); color: var(--app-text-secondary); font: inherit; font-size: 11px; cursor: pointer; }
.entry-status:focus-visible { outline: 2px solid var(--el-color-primary); }
.icon-grid, .file-table-wrap, .column-view, .gallery-view { flex: 1 1 auto; min-height: 0; }
.icon-grid { overflow: auto; align-content: start; display: grid; grid-template-columns: repeat(auto-fill, minmax(112px, 1fr)); gap: 10px; }
.file-tile { min-height: 106px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 5px; padding: 12px 8px; border: 1px solid transparent; border-radius: 10px; background: transparent; color: var(--app-text); font: inherit; cursor: pointer; }
.file-tile:hover, .column-row:hover, .gallery-thumb:hover, .file-table tbody tr:hover { background: var(--app-surface-2); }
.file-tile.is-selected, .gallery-thumb.is-selected { border-color: var(--selection-border); background: var(--selection-bg); }
.file-tile__name { display: -webkit-box; max-width: 100%; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow-wrap: break-word; text-align: center; font-size: 12.5px; font-weight: 500; line-height: 1.35; }
.file-table-wrap { overflow: auto; }
.file-table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
.file-table th { position: sticky; top: 0; z-index: 2; padding: 7px 9px; border-bottom: 1px solid var(--app-border); background: var(--app-surface); color: var(--app-text-muted); font-size: 11.5px; font-weight: 600; text-align: left; }
.file-table td { padding: 7px 9px; border-bottom: 1px solid color-mix(in oklab, var(--app-border) 55%, transparent); color: var(--app-text-secondary); white-space: nowrap; }
.file-table tbody tr { cursor: default; }
.file-table tbody tr.is-selected { background: var(--selection-bg); }
.file-table tbody tr.is-selected td { border-bottom-color: color-mix(in oklab, var(--selection-border) 40%, transparent); }
.file-table tbody tr:last-child td { border-bottom: none; }
.table-name { display: flex; align-items: center; gap: 8px; color: var(--app-text); }
.table-protected { color: var(--app-text-muted); font-size: 11px; }
.column-view { display: flex; align-items: stretch; overflow-x: auto; overflow-y: hidden; }
.column { width: 240px; flex: none; overflow-y: auto; padding: 5px; border-right: 1px solid var(--app-border); }
.column:last-child { border-right: 0; }
.column-row { width: 100%; display: flex; align-items: center; gap: 7px; padding: 6px 8px; border: 0; border-radius: 7px; background: transparent; color: var(--app-text-secondary); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.column-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.column-row.is-selected { color: var(--app-text); background: var(--selection-bg); }
.column-chevron { flex: none; color: var(--app-text-muted); font-size: 15px; }
.column-empty { margin: 8px; font-size: 12px; color: var(--app-text-muted); }
.gallery-view { box-sizing: border-box; height: 100%; display: flex; flex-direction: column; gap: 10px; }
.gallery-stage { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; padding: 16px; border: 1px solid var(--app-border); border-radius: 12px; background: linear-gradient(45deg, color-mix(in oklab, var(--app-surface-2) 55%, transparent) 25%, transparent 25%, 50%, color-mix(in oklab, var(--app-surface-2) 55%, transparent) 50%, 75%, transparent 75%), var(--app-surface-3); background-size: 16px 16px; }
.gallery-stage :deep(> *) { max-width: 100%; max-height: 100%; min-width: 0; min-height: 0; }
.gallery-strip { flex: none; display: flex; gap: 8px; overflow-x: auto; padding: 2px; }
.gallery-thumb { width: 96px; flex: none; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 7px 5px; border: 1px solid transparent; border-radius: 8px; background: transparent; color: var(--app-text-secondary); font: inherit; cursor: pointer; }
.gallery-thumb span { width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; text-align: center; }
.file-empty { margin: 22px 0; text-align: center; font-size: 13px; color: var(--app-text-muted); }
.file-view--placeholder { flex: 1; min-height: 0; overflow: hidden; }
.file-tile:focus-visible, .gallery-thumb:focus-visible, .column-row:focus-visible, .file-table tr:focus-visible { outline: 2px solid var(--el-color-primary); outline-offset: -2px; }
</style>
