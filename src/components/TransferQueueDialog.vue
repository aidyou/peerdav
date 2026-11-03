<script setup>
import { computed } from "vue";
import TransferConflictControls from "./TransferConflictControls.vue";
import { formatBytes } from "../lib/format";
import { statusTagTone } from "../lib/tagTone";
import { t } from "../lib/i18n";
const props = defineProps({ open: Boolean, items: { type: Array, default: () => [] }, running: Boolean, canceling: Boolean,
  summary: { type: String, default: "" }, root: { type: String, default: "" },
  note: { type: String, default: "" }, queueState: { type: Object, default: null }, applyRemaining: Boolean, resolving: Boolean,
  nextOffset: { type: Number, default: null }, pageLoading: Boolean,
});
const emit = defineEmits(["close", "cancel", "reveal", "retry", "resolve", "page", "update:applyRemaining"]);
const statusKey = { pending: "Pending", downloading: "Downloading", done: "Done", canceled: "Canceled", skipped: "Skipped", error: "Error" };

// 已处理 = 已完成 + 已跳过，避免出现“已处理 0/6、跳过 6”这种自相矛盾的统计。
const processed = computed(() => (props.queueState?.completedFiles || 0) + (props.queueState?.skippedFiles || 0));
const total = computed(() => props.queueState?.totalFiles || 0);
const skipped = computed(() => props.queueState?.skippedFiles || 0);
const showProcessed = computed(() => processed.value > 0 || total.value > 0);

// 行内进度只用于正在下载的那一行；分母取该行文件大小（后端已透传）。
const progressFor = (item) => {
  if (item.status !== "downloading") return null;
  const transferred = props.queueState?.currentTransferred;
  if (transferred == null || !item.size) return null;
  return Math.min(100, Math.round((transferred / item.size) * 100));
};
</script>

<template>
  <el-dialog :model-value="open" @update:model-value="!running && emit('close')" width="600" align-center
    :show-close="!running" :close-on-click-modal="false" :close-on-press-escape="!running" data-test="download-dialog"
    class="download-dialog">
    <template #header="{ titleId, titleClass }">
      <div class="queue-header">
        <span :id="titleId" :class="titleClass">{{ t('peer.downloadQueue') }}</span>
        <el-popover placement="bottom-start" trigger="click" :width="320" :show-after="0" popper-class="queue-help-popper">
          <div class="queue-help">
            <p>{{ t('peer.downloadHelp') }}</p>
            <p v-if="note" class="queue-help__note">{{ note }}</p>
          </div>
          <template #reference>
            <button type="button" class="queue-header__help" :aria-label="t('peer.downloadHelpLabel')">?</button>
          </template>
        </el-popover>
      </div>
    </template>

    <div class="queue-dialog">
      <p class="sr-only" role="status">{{ summary }}</p>
      <p v-if="queueState?.currentRelativePath" class="queue-meta" :title="queueState.currentRelativePath">{{ queueState.currentRelativePath }}</p>
      <p v-if="queueState?.error" role="alert" class="queue-error">{{ queueState.error.message }}</p>
      <TransferConflictControls v-if="open && queueState?.conflict" :key="`${queueState.queueId}:${queueState.conflict.conflictId}`" direction="download" :conflict="queueState.conflict" :apply-remaining="applyRemaining" :resolving="resolving || canceling" overwrite-allowed
        @update:apply-remaining="emit('update:applyRemaining', $event)" @resolve="emit('resolve', $event)" @cancel="emit('cancel')" />

      <div class="queue-table" role="table" :aria-label="t('peer.downloadQueue')">
        <!-- 表头对视觉是列名，对读屏是列定义：用 ARIA 表格角色而不是 aria-hidden。 -->
        <div class="queue-table__head" role="row">
          <span role="columnheader">{{ t('peer.queueColPath') }}</span>
          <span role="columnheader">{{ t('peer.queueColSize') }}</span>
          <span role="columnheader">{{ t('peer.queueColStatus') }}</span>
        </div>
        <ul class="transfer-queue download-queue" role="rowgroup">
          <li v-for="item in items" role="row" :key="item.transferId || item.remotePath || item.relativePath" :data-status="item.status"
            :class="{ 'is-active': item.status === 'downloading' }" :data-progress="progressFor(item) ?? undefined">
            <div class="queue-cell queue-cell--path" role="cell" :title="item.relativePath">
              <span class="queue-path">{{ item.relativePath }}</span>
              <details v-if="item.error"><summary>{{ t('transferQueue.details') }}</summary><p>{{ item.error }}</p></details>
            </div>
            <div class="queue-cell queue-cell--size" role="cell">{{ item.size != null ? formatBytes(item.size) : '—' }}</div>
            <div class="queue-cell queue-cell--status" role="cell">
              <el-tag :type="statusTagTone(item.status)" effect="plain" round size="small">{{ t(`peer.downloadStatus${statusKey[item.status] || 'Pending'}`) }}</el-tag>
              <el-button v-if="item.retryable && !running" data-test="retry-download" link type="primary" size="small" @click="emit('retry', item)">{{ t('cloudBrowser.retryDownload') }}</el-button>
            </div>
            <div v-if="progressFor(item) != null" class="queue-row-progress" aria-hidden="true"><span :style="{ width: `${progressFor(item)}%` }"></span></div>
          </li>
        </ul>
      </div>
      <el-button v-if="nextOffset != null" class="load-more" size="small" plain :disabled="pageLoading" data-test="download-next" @click="emit('page', nextOffset)">{{ t('cloudBrowser.uploadNextPage') }}</el-button>
    </div>

    <template #footer>
      <div class="dialog-footer">
        <div class="dialog-footer__stats">
          <span v-if="showProcessed">{{ t('peer.downloadProcessed', { done: processed, total }) }}</span>
          <span v-if="skipped > 0">{{ t('peer.downloadSkipped', { count: skipped }) }}</span>
        </div>
        <div class="dialog-footer__actions">
          <el-button v-if="running" data-test="cancel-download" plain :disabled="canceling" @click="emit('cancel')">{{ t(canceling ? 'peer.cancelingDownload' : 'peer.cancelDownload') }}</el-button>
          <template v-else><el-button v-if="root || queueState?.queueId" plain data-test="reveal-download" @click="emit('reveal')">{{ t('peer.revealInFolder') }}</el-button>
            <el-button type="primary" data-test="close-download" @click="emit('close')">{{ t('common.close') }}</el-button></template>
        </div>
      </div>
    </template>
  </el-dialog>
</template>

<style scoped>
/* 对话框不超过窗口高度：头部/底部固定，内容区自身滚动。 */
:deep(.download-dialog.el-dialog) { display: flex; flex-direction: column; max-height: calc(100vh - 40px); }
:deep(.download-dialog .el-dialog__header),
:deep(.download-dialog .el-dialog__footer) { flex: none; }
:deep(.download-dialog .el-dialog__body) { flex: 1; min-height: 0; overflow-y: auto; }

.queue-header { display: flex; align-items: center; gap: 8px; }
.queue-header__help { display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; border: 1px solid var(--app-border); border-radius: 50%; background: transparent; color: var(--app-text-muted); font-size: 11px; line-height: 1; cursor: pointer; }
.queue-header__help:hover, .queue-header__help:focus-visible { color: var(--app-text); border-color: var(--app-text-muted); outline: none; }
.queue-help { max-width: 300px; font-size: 12.5px; line-height: 1.55; color: var(--app-text-secondary); }
.queue-help p { margin: 0; line-height: 1.55; }
.queue-help__note { margin-top: 6px !important; color: var(--app-text-muted); }

.queue-dialog { display: flex; flex-direction: column; gap: 10px; }
.queue-dialog p.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
.queue-dialog p { margin: 0; font-size: 13px; overflow-wrap: anywhere; }
.queue-meta { font-size: 12px; color: var(--app-text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.queue-error { color: var(--el-color-danger); }

/* 表头与数据行共用同一列模板：状态固定收窄、大小稍宽、路径吃掉剩余空间。 */
.queue-table__head,
.transfer-queue li { display: grid; grid-template-columns: minmax(0, 1fr) 108px 84px; align-items: center; gap: 10px; }
.queue-table__head { padding: 6px 19px 6px 11px; color: var(--app-text-muted); font-size: 11.5px; font-weight: 600; }

.transfer-queue { list-style: none; max-height: min(360px, 45vh); overflow-y: auto; overscroll-behavior: contain; scrollbar-gutter: stable; margin: 0; padding: 0; border: 1px solid var(--app-border); border-radius: 10px; }
.transfer-queue li { position: relative; padding: 9px 11px; }
.transfer-queue li + li { border-top: 1px solid var(--app-border); }
/* 正在下载的行用主色 20% 不透明背景高亮。 */
.transfer-queue li.is-active { background: color-mix(in oklab, var(--el-color-primary) 20%, transparent); }

.queue-cell { min-width: 0; }
.queue-cell--path { display: flex; flex-direction: column; gap: 2px; }
.queue-path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; color: var(--app-text); }
.queue-cell--size { font-size: 12px; color: var(--app-text-muted); font-variant-numeric: tabular-nums; }
.queue-cell--status { display: flex; align-items: center; justify-content: flex-end; gap: 6px; }
.queue-cell--status :deep(.el-button) { margin-left: 0; }

/* 行内进度条：贴在当前行底部，宽度即完成百分比。 */
.queue-row-progress { position: absolute; left: 0; right: 0; bottom: 0; height: 3px; background: color-mix(in oklab, var(--el-color-primary) 18%, transparent); pointer-events: none; }
.queue-row-progress span { display: block; height: 100%; background: var(--el-color-primary); transition: width 0.2s ease; }

details { font-size: 12px; color: var(--app-text-muted); overflow-wrap: anywhere; }
summary { cursor: pointer; }
.load-more { align-self: flex-start; }

/* footer：统计靠左、操作靠右；gap 明确保证“打开所在目录/关闭”之间有间隔。 */
.dialog-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.dialog-footer__stats { display: flex; align-items: center; gap: 14px; min-width: 0; font-size: 12px; color: var(--app-text-muted); font-variant-numeric: tabular-nums; }
.dialog-footer__actions { flex: none; display: flex; align-items: center; gap: 8px; }
.dialog-footer__actions :deep(.el-button + .el-button) { margin-left: 0; }

@media (max-width: 540px) {
  .queue-table__head,
  .transfer-queue li { grid-template-columns: minmax(0, 1fr) 72px 108px; }
  .dialog-footer { align-items: flex-start; flex-direction: column; }
  .dialog-footer__actions { width: 100%; justify-content: flex-end; }
}
</style>