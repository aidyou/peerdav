<script setup>
import { computed } from "vue";
import FileGlyph from "./FileGlyph.vue";
import { formatBytes } from "../lib/format";
import { t } from "../lib/i18n";
const props = defineProps({
  open: Boolean, summary: { type: Object, default: null }, page: { type: Object, default: null },
  running: Boolean, pageLoading: Boolean, resolving: Boolean, overwriteAllowed: Boolean,
  batchError: { type: String, default: "" }, pageNotice: { type: String, default: "" }, applyRemaining: Boolean,
});
const emit = defineEmits(["close", "cancel", "page", "resolve", "update:applyRemaining"]);
const skipped = computed(() => t("cloudBrowser.uploadSkipped", { empty: props.summary?.skippedEmptyDirectories ?? 0, links: props.summary?.skippedSymlinks ?? 0 }));
</script>

<template>
  <el-dialog :model-value="open" @update:model-value="emit('close')" :title="t('peer.uploadQueue')" width="560" align-center
    :show-close="!running" :close-on-click-modal="false" :close-on-press-escape="false" data-test="upload-dialog">
    <div class="transfer-dialog upload-dialog">
      <p>{{ t('cloudBrowser.uploadStreaming') }}</p>
      <template v-if="summary">
        <p data-test="upload-summary">{{ t('cloudBrowser.uploadBatchSummary', { count: summary.completedFiles, bytes: formatBytes(summary.completedBytes), skipped: summary.skippedFiles }) }} · {{ t(`cloudBrowser.phase.${summary.phase}`) }}</p>
        <p v-if="summary.currentRelativePath">{{ t('cloudBrowser.uploadCurrent') }}: {{ summary.currentRelativePath }}</p>
        <p data-test="upload-skipped">{{ skipped }}</p>
        <p v-if="summary.recoveryRequired" role="alert">{{ t('cloudBrowser.uploadRecovery') }}</p>
        <div v-if="summary.conflict" data-test="upload-conflict">
          <p>{{ t('cloudBrowser.uploadConflict') }}: {{ summary.conflict.relativePath }}</p>
          <p class="mono">{{ JSON.stringify(summary.conflict.key) }}<template v-if="summary.conflict.etag != null"> · ETag: {{ JSON.stringify(summary.conflict.etag) }}</template></p>
          <label><input :checked="applyRemaining" @change="emit('update:applyRemaining', $event.target.checked)" data-test="upload-apply-remaining" type="checkbox"> {{ t('cloudBrowser.uploadApplyRemaining') }}</label>
          <div>
            <el-button data-test="upload-skip" :disabled="resolving" @click="emit('resolve', 'skip')">{{ t('cloudBrowser.uploadSkip') }}</el-button>
            <el-button data-test="upload-overwrite" :disabled="resolving || !overwriteAllowed" @click="emit('resolve', 'overwrite')">{{ t('cloudBrowser.uploadOverwriteAction') }}</el-button>
            <el-button data-test="upload-conflict-cancel" :disabled="resolving" @click="emit('resolve', 'cancel')">{{ t('common.cancel') }}</el-button>
          </div>
        </div>
      </template>
      <p v-if="batchError" role="alert">{{ batchError }}</p>
      <p v-if="pageNotice" role="status">{{ pageNotice }}</p>
      <p>{{ t('cloudBrowser.uploadPageOnly') }}</p>
      <div>
        <el-button data-test="upload-up" :disabled="pageLoading || !page?.parentToken" @click="emit('page', page.parentToken)">{{ t('cloudBrowser.uploadUp') }}</el-button>
        <el-button data-test="upload-page-reset" :disabled="pageLoading" @click="emit('page')">{{ t('cloudBrowser.refresh') }}</el-button>
        <span>{{ page?.relativePath || summary?.rootName }}</span>
      </div>
      <ul class="transfer-queue download-queue">
        <li v-for="item in page?.entries || []" :key="item.relativePath" :data-status="item.relativePath === summary?.currentRelativePath && running ? 'active' : 'unscanned'">
          <div class="transfer-queue__name download-queue__name">
            <FileGlyph :entry="{ isDir: item.kind === 'directory', name: item.name }" />
            <div><strong>{{ item.name }}</strong><span>{{ t(`cloudBrowser.uploadKind.${item.kind}`) }} · {{ item.kind === 'file' ? formatBytes(item.size) : '—' }}</span></div>
          </div>
          <el-button v-if="item.kind === 'directory'" data-test="upload-enter" :disabled="pageLoading || !item.directoryToken" @click="emit('page', item.directoryToken)">{{ t('cloudBrowser.uploadEnter') }}</el-button>
          <span v-else>{{ item.relativePath === summary?.currentRelativePath && running ? t('cloudBrowser.uploadActive') : t('cloudBrowser.uploadUnscanned') }}</span>
        </li>
      </ul>
      <el-button v-if="page?.nextCursor" data-test="upload-next" :disabled="pageLoading" @click="emit('page', page.directoryToken, page.nextCursor)">{{ t('cloudBrowser.uploadNextPage') }}</el-button>
    </div>
    <template #footer>
      <el-button v-if="running" data-test="cancel-upload" plain @click="emit('cancel')">{{ t('peer.cancelUpload') }}</el-button>
      <el-button v-else data-test="close-upload" type="primary" @click="emit('close')">{{ t('common.close') }}</el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
.transfer-dialog { display: flex; flex-direction: column; gap: 12px; }
.transfer-dialog p { margin: 0; font-size: 12.5px; overflow-wrap: anywhere; }
.transfer-queue { list-style: none; max-height: min(320px, 34vh); overflow-y: auto; overscroll-behavior: contain; margin: 0; padding: 0; border: 1px solid var(--app-border); border-radius: 10px; }
.transfer-queue li { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 9px 11px; }
.transfer-queue li + li { border-top: 1px solid var(--app-border); }
.transfer-queue__name { display: flex; align-items: center; gap: 9px; min-width: 0; }
.transfer-queue__name > div { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.transfer-queue__name strong, .transfer-queue__name span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.transfer-queue__name strong { font-size: 12.5px; color: var(--app-text); }
.transfer-queue__name span { font-size: 11px; color: var(--app-text-muted); }
</style>
