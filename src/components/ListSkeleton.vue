<script setup>
import { t } from "../lib/i18n";

// 列表在数据到达前不能是空白：空白会被读成「没有内容」，骨架屏才表达「正在读取」。
// Element Plus 的折叠面板本身没有骨架屏模式，所以这里用同尺寸的占位行整体替换面板，
// 外框圆角、行内边距与真实列表一致，数据到达后换成真实行不会跳动。
const props = defineProps({
  rows: { type: Number, default: 3 },
  // collapse: 设置页折叠列表外框（8px 圆角、10/15 内边距）
  // card: 首页服务器卡片列表外框（14px 圆角、12/18 内边距）
  variant: {
    type: String,
    default: "collapse",
    validator: (value) => value === "collapse" || value === "card",
  },
});
</script>

<template>
  <div
    class="list-skeleton"
    :class="`list-skeleton--${props.variant}`"
    role="status"
    :aria-busy="true"
    :aria-label="t('common.loading')"
  >
    <el-skeleton animated :count="props.rows">
      <template #template>
        <div class="list-skeleton__row">
          <el-skeleton-item variant="rect" class="list-skeleton__icon" />
          <el-skeleton-item variant="text" class="list-skeleton__title" />
          <el-skeleton-item variant="text" class="list-skeleton__pill" />
          <el-skeleton-item
            variant="text"
            class="list-skeleton__pill list-skeleton__pill--short"
          />
          <el-skeleton-item variant="text" class="list-skeleton__chevron" />
        </div>
      </template>
    </el-skeleton>
  </div>
</template>

<style scoped>
.list-skeleton {
  border: 1px solid var(--app-border);
  overflow: hidden;
}

.list-skeleton--collapse {
  border-radius: 8px;
}

.list-skeleton--card {
  border-radius: 14px;
  background: var(--app-surface);
}

.list-skeleton :deep(.el-skeleton) {
  display: flex;
  flex-direction: column;
}

.list-skeleton__row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 15px;
}

.list-skeleton--card .list-skeleton__row {
  gap: 16px;
  padding: 12px 14px 12px 18px;
}

.list-skeleton__row + .list-skeleton__row {
  border-top: 1px solid var(--app-border);
}

.list-skeleton__icon {
  flex: none;
  width: 32px;
  height: 32px;
  border-radius: 8px;
}

.list-skeleton__title {
  flex: 1;
  min-width: 0;
  height: 14px;
}

.list-skeleton__pill {
  flex: none;
  width: 56px;
  height: 14px;
  border-radius: 999px;
}

.list-skeleton__pill--short {
  width: 42px;
}

.list-skeleton__chevron {
  flex: none;
  width: 12px;
  height: 12px;
  border-radius: 3px;
}
</style>
