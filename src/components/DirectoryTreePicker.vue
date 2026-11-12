<script setup>
import { computed, ref, watch } from 'vue';
import FileGlyph from './FileGlyph.vue';
import { t } from '../lib/i18n';

const props = defineProps({
  open: Boolean,
  modelValue: { type: String, default: '' },
  rootLabel: { type: String, default: '/' },
  loadChildren: { type: Function, required: true },
});
const emit = defineEmits(['update:modelValue', 'loading', 'error']);
const children = ref(new Map());
const expanded = ref(new Set());
const loading = ref(new Set());
const failures = ref(new Set());
let generation = 0;
const rows = computed(() => {
  const result = [];
  function visit(node, depth, ancestors) {
    if (ancestors.has(node.id)) return;
    result.push({ ...node, depth });
    if (expanded.value.has(node.id)) {
      const next = new Set(ancestors); next.add(node.id);
      for (const child of children.value.get(node.id) || []) visit(child, depth + 1, next);
    }
  }
  visit({ id: '', name: props.rootLabel, isDir: true }, 0, new Set());
  return result;
});
async function expand(id) {
  if (loading.value.has(id)) return;
  if (expanded.value.has(id)) { expanded.value = new Set([...expanded.value].filter(key => key !== id)); return; }
  if (!children.value.has(id)) {
    const version = generation;
    loading.value = new Set([...loading.value, id]); emit('loading', true);
    failures.value = new Set([...failures.value].filter(key => key !== id));
    try {
      const entries = await props.loadChildren(id);
      if (version !== generation) return;
      children.value = new Map(children.value).set(id, entries);
    } catch (error) {
      if (version === generation) { failures.value = new Set([...failures.value, id]); emit('error', error); }
      return;
    } finally {
      if (version === generation) { loading.value = new Set([...loading.value].filter(key => key !== id)); emit('loading', loading.value.size > 0); }
    }
  }
  expanded.value = new Set([...expanded.value, id]);
}
function select(row) { emit('update:modelValue', row.id); if (!expanded.value.has(row.id)) void expand(row.id); }
function keydown(event, row) {
  if (event.key === 'ArrowRight') { event.preventDefault(); if (!expanded.value.has(row.id)) void expand(row.id); }
  if (event.key === 'ArrowLeft') { event.preventDefault(); expanded.value = new Set([...expanded.value].filter(id => id !== row.id)); }
}
watch(() => props.open, open => {
  generation++; children.value = new Map(); expanded.value = new Set(); loading.value = new Set(); failures.value = new Set(); emit('loading', false);
  if (open) { emit('update:modelValue', ''); void expand(''); }
}, { immediate: true });
</script>

<template>
  <div class="directory-tree" role="tree" :aria-label="t('drive.moveDestination')">
    <div v-for="row in rows" :key="row.id" class="directory-tree__row" role="treeitem" :aria-level="row.depth + 1" :aria-selected="modelValue === row.id" :aria-expanded="expanded.has(row.id)" :aria-busy="loading.has(row.id)" :style="{ paddingInlineStart: `${row.depth * 20}px` }" @keydown="keydown($event, row)">
      <button type="button" class="directory-tree__toggle" :aria-label="row.name" :aria-expanded="expanded.has(row.id)" :disabled="loading.has(row.id)" @click="expand(row.id)">{{ loading.has(row.id) ? '…' : expanded.has(row.id) ? '▾' : '▸' }}</button>
      <button type="button" class="directory-tree__label" :data-directory="row.id" @click="select(row)"><FileGlyph :entry="row"/><span>{{ row.name }}</span></button>
      <span v-if="failures.has(row.id)" role="status">{{ t('cloud.phaseFailed') }}</span>
    </div>
  </div>
</template>

<style scoped>
.directory-tree { max-height: min(420px, 55vh); overflow: auto; border: 1px solid var(--app-border); border-radius: 8px; padding: 6px; }
.directory-tree__row { display: flex; align-items: center; min-height: 32px; border-radius: 5px; }
.directory-tree__row[aria-selected=true] { background: var(--app-surface-2); outline: 1px solid var(--app-border); }
.directory-tree button { border: 0; background: transparent; color: var(--app-text); font: inherit; cursor: pointer; min-height: 30px; }
.directory-tree button:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.directory-tree__toggle { width: 30px; flex: none; }
.directory-tree__label { flex: 1; display: flex; align-items: center; gap: 8px; text-align: start; min-width: 0; }
.directory-tree__label span { overflow-wrap: anywhere; }
.directory-tree__row:hover { background: var(--app-surface-2); }
</style>
