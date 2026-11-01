<script setup>
import { t } from "../lib/i18n";

defineProps({
  mode: { type: String, default: "list", validator: (value) => ["icons", "list", "columns", "gallery"].includes(value) },
  compact: { type: Boolean, default: false },
});
</script>

<template>
  <div class="browser-skeleton" :class="[`browser-skeleton--${mode}`, { 'browser-skeleton--compact': compact }]" role="status" aria-busy="true" :aria-label="t('common.loading')">
    <el-skeleton animated aria-hidden="true">
      <template #template>
        <div v-if="mode === 'gallery' && !compact" class="browser-skeleton__stage"><el-skeleton-item variant="image" /></div>
        <div class="browser-skeleton__items">
          <div v-for="index in (compact ? 1 : 6)" :key="index" class="browser-skeleton__entry">
            <el-skeleton-item variant="rect" class="browser-skeleton__icon" />
            <el-skeleton-item variant="text" class="browser-skeleton__name" />
            <el-skeleton-item v-if="mode === 'list' && !compact" variant="text" class="browser-skeleton__meta" />
          </div>
        </div>
      </template>
    </el-skeleton>
  </div>
</template>

<style scoped>
.browser-skeleton { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px; overflow: hidden; --el-skeleton-color: var(--app-surface-2); --el-skeleton-to-color: var(--app-surface-3); }
.browser-skeleton__items { display: flex; flex-direction: column; gap: 12px; }
.browser-skeleton__entry { display: flex; align-items: center; gap: 8px; min-width: 0; }
.browser-skeleton__icon { flex: none; width: 22px; height: 22px; border-radius: 5px; }
.browser-skeleton__name { flex: 1; min-width: 0; max-width: 180px; height: 12px; }
.browser-skeleton__meta { flex: none; width: 20%; height: 12px; margin-left: auto; }
.browser-skeleton--icons .browser-skeleton__items { display: grid; grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)); gap: 16px; }
.browser-skeleton--icons .browser-skeleton__entry, .browser-skeleton--gallery .browser-skeleton__entry { flex-direction: column; padding: 12px 8px; }
.browser-skeleton--icons .browser-skeleton__icon, .browser-skeleton--gallery .browser-skeleton__icon { width: 48px; height: 48px; }
.browser-skeleton--icons .browser-skeleton__name, .browser-skeleton--gallery .browser-skeleton__name { flex: none; width: 70%; }
.browser-skeleton__stage { height: clamp(120px, 30vh, 280px); margin-bottom: 12px; }
.browser-skeleton__stage .el-skeleton__item { width: 100%; height: 100%; }
.browser-skeleton--gallery .browser-skeleton__items { display: grid; grid-template-columns: repeat(6, minmax(0, 1fr)); }
.browser-skeleton--compact { flex: none; width: 112px; padding: 4px; }
.browser-skeleton--compact .browser-skeleton__icon { width: 16px; height: 16px; }
@media (prefers-reduced-motion: reduce) {
  .browser-skeleton :deep(.el-skeleton__item) { animation: none !important; }
}
</style>
