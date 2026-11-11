<script setup>
import TransferConflictControls from "./TransferConflictControls.vue";
import { computed } from "vue";
import FileGlyph from "./FileGlyph.vue";
import { formatBytes } from "../lib/format";
import { statusTagTone } from "../lib/tagTone";
import { t } from "../lib/i18n";
const props = defineProps({
  open: Boolean, summary: { type: Object, default: null }, page: { type: Object, default: null },
  running: Boolean, resolving: Boolean, overwriteAllowed: Boolean,
  batchError: { type: String, default: "" }, pageNotice: { type: String, default: "" }, applyRemaining: Boolean,
});
const emit = defineEmits(["close", "cancel", "resolve", "update:applyRemaining"]);

// 扫描阶段跳过的空目录/符号链接只在真的发生时写进帮助说明。
const skippedEmpty = computed(() => props.summary?.skippedEmptyDirectories ?? 0);
const skippedLinks = computed(() => props.summary?.skippedSymlinks ?? 0);
const hasScanSkips = computed(() => skippedEmpty.value + skippedLinks.value > 0);
const scanSkipsText = computed(() => t("cloudBrowser.uploadSkipped", { empty: skippedEmpty.value, links: skippedLinks.value }));

// 批次统计与下载队列保持一致：已处理 = 已完成 + 已跳过，总量由后端扫描时累计。
const processed = computed(() => (props.summary?.completedFiles || 0) + (props.summary?.skippedFiles || 0));
const total = computed(() => props.summary?.totalFiles || 0);
const uploadedBytes = computed(() => props.summary?.completedBytes || 0);
// 有统计内容时才作为 aria-live 区域，避免空区域被读屏登记为状态源。
const hasStats = computed(() => total.value > 0 || uploadedBytes.value > 0 || (props.summary?.skippedFiles ?? 0) > 0);

// 列表是远端目录预览而非逐文件队列，只有正在处理的条目才有状态可言。
const isActive = (item) => item.relativePath === props.summary?.currentRelativePath && props.running;
// 上传目标目录固定为选择时的目录，预览页只展示它的直接子项。
const remotePath = computed(() => props.page?.relativePath || props.summary?.rootName || "");
// 目录行右侧状态图标：进行中旋转、完成打勾、失败红叉；取消不额外标注。
const phaseIcon = computed(() => {
  const phase = props.summary?.phase;
  if (phase === "completed") return "done";
  if (phase === "failed" || phase === "unknown") return "error";
  if (!phase || phase === "cancelled" || phase === "cancelling") return null;
  return "active";
});
const entrySize = (item) => {
  if (item.kind === "file") return formatBytes(item.size);
  if (item.kind === "directory") return "—";
  return t(`cloudBrowser.uploadKind.${item.kind}`);
};
</script>

<template>
  <el-dialog :model-value="open" @update:model-value="!running && emit('close')" width="600" align-center class="upload-dialog"
    :show-close="!running" :close-on-click-modal="false" :close-on-press-escape="!running" data-test="upload-dialog">
    <template #header="{ titleId, titleClass }">
      <div class="queue-header">
        <span :id="titleId" :class="titleClass">{{ t('peer.uploadQueue') }}</span>
        <el-popover placement="bottom-start" trigger="click" :width="320" :show-after="0" popper-class="queue-help-popper">
          <div class="queue-help">
            <p>{{ t('cloudBrowser.uploadStreaming') }}</p>
            <p>{{ t('cloudBrowser.uploadPageOnly') }}</p>
            <p v-if="hasScanSkips">{{ scanSkipsText }}</p>
          </div>
          <template #reference>
            <button type="button" class="queue-header__help" :aria-label="t('peer.uploadHelpLabel')">?</button>
          </template>
        </el-popover>
      </div>
    </template>

    <div class="transfer-dialog upload-dialog">

      <!-- ① 传输状态：阶段已由目录行右侧图标表达，这里只保留当前文件路径。 -->
      <div v-if="summary?.currentRelativePath" class="upload-summary" data-test="upload-summary">
        <p class="upload-summary__current">{{ t('cloudBrowser.uploadCurrent') }} <span class="mono">{{ summary.currentRelativePath }}</span></p>
      </div>

      <!-- ② 需要处理：只在出现时以强调条呈现，冲突走独立浮层 -->
      <TransferConflictControls v-if="open && summary?.conflict" :key="`${summary.batchId}:${summary.conflict.conflictId}`" :conflict="summary.conflict" :apply-remaining="applyRemaining" :resolving="resolving" :overwrite-allowed="overwriteAllowed"
        @update:apply-remaining="emit('update:applyRemaining', $event)" @resolve="emit('resolve', $event)" @cancel="emit('cancel')" />
      <p v-if="summary?.recoveryRequired" role="alert" class="upload-alert">{{ t('cloudBrowser.uploadRecovery') }}</p>
      <p v-if="batchError" role="alert" class="upload-alert">{{ batchError }}</p>

      <!-- ③ 远端目录预览：目录固定，只展示直接子项；不提供导航，避免与批次扫描竞态 -->
      <section class="upload-dir">
        <p class="upload-dir__path">
          <span class="upload-dir__label">{{ t('cloudBrowser.uploadRemoteDir') }}</span>
          <span class="mono upload-dir__value" :title="remotePath">{{ remotePath }}</span>
          <span v-if="phaseIcon" class="upload-dir__state" :class="`is-${phaseIcon}`">
            <span v-if="phaseIcon === 'active'" class="upload-dir__spinner" aria-hidden="true">
              <svg viewBox="0 0 16 16">
                <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="24 14" />
              </svg>
            </span>
            <svg v-else-if="phaseIcon === 'done'" viewBox="0 0 16 16" aria-hidden="true">
              <path d="M3.5 8.5l3 3 5.5-6.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
            <svg v-else viewBox="0 0 16 16" aria-hidden="true">
              <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
            </svg>
            <!-- 图标旁写明状态文字：动画可能被系统“减少动效”关闭，状态不能只靠旋转表达。 -->
            <span class="upload-dir__state-text">{{ t(`cloudBrowser.phase.${summary.phase}`) }}</span>
          </span>
        </p>
        <div class="queue-table" role="table" :aria-label="t('peer.uploadQueue')">
          <!-- 与下载队列一致：表头既是视觉列名，也是读屏的列定义。 -->
          <div class="queue-table__head" role="row">
            <span role="columnheader">{{ t('peer.queueColPath') }}</span>
            <span role="columnheader">{{ t('peer.queueColSize') }}</span>
            <span role="columnheader">{{ t('peer.queueColStatus') }}</span>
          </div>
          <ul class="transfer-queue upload-queue" role="rowgroup">
            <li v-for="item in page?.entries || []" role="row" :key="item.relativePath" :data-status="isActive(item) ? 'active' : 'unscanned'"
              :class="{ 'is-active': isActive(item) }">
              <div class="queue-cell queue-cell--path" role="cell" :title="item.relativePath">
                <FileGlyph :entry="{ isDir: item.kind === 'directory', name: item.name }" />
                <span class="queue-path">{{ item.name }}</span>
              </div>
              <div class="queue-cell queue-cell--size" role="cell">{{ entrySize(item) }}</div>
              <div class="queue-cell queue-cell--status" role="cell">
                <el-tag v-if="isActive(item)" :type="statusTagTone('uploading')" effect="plain" round size="small">{{ t('cloudBrowser.uploadActive') }}</el-tag>
                <span v-else class="queue-empty">—</span>
              </div>
            </li>
          </ul>
        </div>
        <p v-if="pageNotice" role="status" class="upload-dir__notice">{{ pageNotice }}</p>
      </section>
    </div>

    <template #footer>
      <div class="dialog-footer">
        <div class="dialog-footer__stats" :role="hasStats ? 'status' : undefined">
          <span v-if="total > 0">{{ t('peer.uploadProcessed', { done: processed, total }) }}</span>
          <span v-if="uploadedBytes > 0">{{ t('peer.uploadUploaded', { bytes: formatBytes(uploadedBytes) }) }}</span>
          <span v-if="(summary?.skippedFiles ?? 0) > 0">{{ t('peer.uploadSkippedFiles', { count: summary.skippedFiles }) }}</span>
        </div>
        <div class="dialog-footer__actions">
          <el-button v-if="running" data-test="cancel-upload" plain @click="emit('cancel')">{{ t('peer.cancelUpload') }}</el-button>
          <el-button v-else data-test="close-upload" type="primary" @click="emit('close')">{{ t('common.close') }}</el-button>
        </div>
      </div>
    </template>
  </el-dialog>
</template>

<style scoped>
/* 对话框不超过窗口高度：头部/底部固定，内容区自身滚动。 */
:deep(.upload-dialog.el-dialog) { display: flex; flex-direction: column; max-height: calc(100vh - 40px); }
:deep(.upload-dialog .el-dialog__header),
:deep(.upload-dialog .el-dialog__footer) { flex: none; }
:deep(.upload-dialog .el-dialog__body) { flex: 1; min-height: 0; overflow-y: auto; }

.queue-header { display: flex; align-items: center; gap: 8px; }
.queue-header__help { display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; border: 1px solid var(--app-border); border-radius: 50%; background: transparent; color: var(--app-text-muted); font-size: 11px; line-height: 1; cursor: pointer; }
.queue-header__help:hover, .queue-header__help:focus-visible { color: var(--app-text); border-color: var(--app-text-muted); outline: none; }
.queue-help { max-width: 300px; font-size: 12.5px; line-height: 1.55; color: var(--app-text-secondary); }
.queue-help p { margin: 0; line-height: 1.55; }

.transfer-dialog { display: flex; flex-direction: column; gap: 14px; }
.transfer-dialog p { margin: 0; overflow-wrap: anywhere; }

/* ① 传输状态：阶段由目录行图标表达，这里只显示当前文件。 */
.upload-summary { display: flex; flex-direction: column; gap: 5px; }
.upload-summary__current { display: flex; align-items: baseline; gap: 6px; font-size: 12px; color: var(--app-text-secondary); }
.mono { font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace; font-size: 11.5px; color: var(--app-text-muted); overflow-wrap: anywhere; }

/* ② 需要处理：唯一的强调容器，左色条 + 极浅背景，与中性状态区分。 */
.upload-alert { padding: 8px 10px; border-left: 3px solid var(--el-color-danger); border-radius: 6px; background: color-mix(in oklab, var(--el-color-danger) 7%, transparent); color: var(--el-color-danger); font-size: 12.5px; line-height: 1.5; }

/* ③ 远端目录预览：目录固定，只保留一行路径与直接子项列表。 */
.upload-dir { display: flex; flex-direction: column; gap: 8px; }
.upload-dir__path { display: flex; align-items: center; gap: 2px; min-width: 0; font-size: 12.5px; color: var(--app-text-muted); }
.upload-dir__label { flex: none; }
.upload-dir__value { flex: 1; min-width: 0; font-size: 12px; color: var(--app-text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.upload-dir__state { flex: none; display: inline-flex; align-items: center; gap: 5px; margin-left: 6px; }
.upload-dir__state svg { display: block; width: 16px; height: 16px; flex: none; }
.upload-dir__state-text { font-size: 12px; line-height: 16px; }
.upload-dir__state.is-active { color: var(--el-color-primary); }
.upload-dir__state.is-done { color: var(--el-color-success); }
.upload-dir__state.is-error { color: var(--el-color-danger); }
/* 在固定大小的 HTML 盒子上旋转，避免依赖不同 WebView 的 SVG transform-box 实现。 */
.upload-dir__spinner { display: inline-flex; width: 16px; height: 16px; flex: none; transform-origin: center; animation: upload-directory-spin 0.9s linear infinite; }
@keyframes upload-directory-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
/* 旋转只是“进行中”的辅助表达，用户要求减少动效时改为静态图标。 */
@media (prefers-reduced-motion: reduce) {
  .upload-dir__spinner { animation: none; }
}
.upload-dir__notice { font-size: 11.5px; color: var(--app-text-muted); }

/* 表头与数据行共用同一列模板：状态固定收窄、大小稍宽、路径吃掉剩余空间。 */
.queue-table__head,
.transfer-queue li { display: grid; grid-template-columns: minmax(0, 1fr) 108px 84px; align-items: center; gap: 10px; }
.queue-table__head { padding: 6px 19px 6px 11px; color: var(--app-text-muted); font-size: 11.5px; font-weight: 600; }

.transfer-queue { list-style: none; max-height: min(360px, 45vh); overflow-y: auto; overscroll-behavior: contain; scrollbar-gutter: stable; margin: 0; padding: 0; border: 1px solid var(--app-border); border-radius: 10px; }
.transfer-queue li { padding: 9px 11px; }
.transfer-queue li + li { border-top: 1px solid var(--app-border); }
.transfer-queue li.is-active { background: color-mix(in oklab, var(--el-color-primary) 20%, transparent); }

.queue-cell { min-width: 0; }
.queue-cell--path { display: flex; align-items: center; gap: 9px; }
.queue-path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; color: var(--app-text); }
.queue-cell--size { font-size: 12px; color: var(--app-text-muted); font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.queue-cell--status { display: flex; align-items: center; justify-content: flex-end; gap: 6px; }
.queue-cell--status :deep(.el-button) { margin-left: 0; }
.queue-empty { font-size: 12px; color: var(--app-text-muted); }

/* footer：统计靠左、操作靠右；gap 明确保证按钮之间有间隔。 */
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