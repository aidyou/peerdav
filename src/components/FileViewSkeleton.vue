<script setup>
import { t } from "../lib/i18n";

// Directory loads must keep the same shape as the real listing: a blank pane
// reads as "empty folder" and collapses the layout, which makes navigation
// jump. Every variant mirrors the view it replaces.
const props = defineProps({
  variant: {
    type: String,
    default: "list",
    validator: (value) => ["list", "icons", "gallery", "columns"].includes(value),
  },
  rows: { type: Number, default: 8 },
});
</script>

<template>
  <div
    class="file-skeleton"
    :class="`file-skeleton--${props.variant}`"
    role="status"
    :aria-busy="true"
    :aria-label="t('common.loading')"
  >
    <template v-if="props.variant === 'icons'">
      <div class="file-skeleton__grid">
        <div v-for="row in props.rows" :key="row" class="file-skeleton__tile">
          <span class="file-skeleton__block file-skeleton__tile-icon" />
          <span class="file-skeleton__block file-skeleton__line" />
        </div>
      </div>
    </template>
    <template v-else-if="props.variant === 'gallery'">
      <span class="file-skeleton__block file-skeleton__stage" />
      <div class="file-skeleton__strip">
        <div v-for="row in props.rows" :key="row" class="file-skeleton__thumb">
          <span class="file-skeleton__block file-skeleton__thumb-icon" />
          <span class="file-skeleton__block file-skeleton__line" />
        </div>
      </div>
    </template>
    <template v-else-if="props.variant === 'columns'">
      <div class="file-skeleton__column">
        <div v-for="row in props.rows" :key="row" class="file-skeleton__row">
          <span class="file-skeleton__block file-skeleton__icon" />
          <span class="file-skeleton__block file-skeleton__line" />
        </div>
      </div>
    </template>
    <template v-else>
      <div class="file-skeleton__head">
        <span class="file-skeleton__block file-skeleton__line" />
        <span class="file-skeleton__block file-skeleton__line file-skeleton__line--short" />
        <span class="file-skeleton__block file-skeleton__line file-skeleton__line--short" />
      </div>
      <div v-for="row in props.rows" :key="row" class="file-skeleton__row">
        <span class="file-skeleton__block file-skeleton__icon" />
        <span class="file-skeleton__block file-skeleton__line" />
        <span class="file-skeleton__block file-skeleton__line file-skeleton__line--short" />
        <span class="file-skeleton__block file-skeleton__line file-skeleton__line--short" />
      </div>
    </template>
  </div>
</template>

<style scoped>
.file-skeleton { min-height: 0; display: flex; flex-direction: column; gap: 4px; overflow: hidden; }
.file-skeleton__block {
  display: block;
  border-radius: 6px;
  background: linear-gradient(90deg, var(--app-surface-2) 25%, var(--app-surface-3) 37%, var(--app-surface-2) 63%);
  background-size: 400% 100%;
  animation: file-skeleton-pulse 1.4s ease infinite;
}
.file-skeleton__head,
.file-skeleton__row { display: flex; align-items: center; gap: 10px; padding: 7px 9px; }
.file-skeleton__head { border-bottom: 1px solid var(--app-border); padding-top: 2px; }
.file-skeleton__head .file-skeleton__line { height: 10px; }
.file-skeleton__row { border-bottom: 1px solid color-mix(in oklab, var(--app-border) 55%, transparent); }
.file-skeleton__icon { flex: none; width: 18px; height: 18px; }
.file-skeleton__line { flex: 1; min-width: 0; height: 13px; }
.file-skeleton__line--short { flex: none; width: 64px; }
.file-skeleton__grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(112px, 1fr));
  gap: 10px;
  padding: 2px;
  overflow: hidden;
}
.file-skeleton__tile {
  min-height: 106px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 9px;
  padding: 12px 8px;
}
.file-skeleton__tile-icon { width: 34px; height: 34px; border-radius: 9px; }
.file-skeleton__tile .file-skeleton__line { flex: none; width: 68%; }
.file-skeleton__stage { flex: 1; min-height: 120px; border-radius: 12px; }
.file-skeleton__strip { flex: none; display: flex; gap: 8px; padding: 2px; overflow: hidden; }
.file-skeleton__thumb {
  width: 96px;
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 7px 5px;
}
.file-skeleton__thumb-icon { width: 26px; height: 26px; border-radius: 7px; }
.file-skeleton__thumb .file-skeleton__line { flex: none; width: 78%; height: 10px; }
.file-skeleton__column { display: flex; flex-direction: column; gap: 4px; }
@keyframes file-skeleton-pulse {
  0% { background-position: 100% 50%; }
  100% { background-position: 0 50%; }
}
@media (prefers-reduced-motion: reduce) {
  .file-skeleton__block { animation: none; }
}
</style>