<script setup>
import TransferConflictControls from "./TransferConflictControls.vue";
import FileGlyph from "./FileGlyph.vue";
import { formatBytes } from "../lib/format";
import { statusTagTone } from "../lib/tagTone";
import { t } from "../lib/i18n";
defineProps({ open: Boolean, items: { type: Array, default: () => [] }, running: Boolean, canceling: Boolean,
  summary: { type: String, default: "" }, root: { type: String, default: "" },
  percentage: { type: Number, default: null }, transferred: { type: Number, default: null }, total: { type: Number, default: null },
  note: { type: String, default: "" }, queueState: { type: Object, default: null }, applyRemaining: Boolean, resolving: Boolean,
  nextOffset: { type: Number, default: null }, pageLoading: Boolean,
});
const emit = defineEmits(["close", "cancel", "reveal", "retry", "resolve", "page", "update:applyRemaining"]);
const statusKey = { pending: "Pending", downloading: "Downloading", done: "Done", canceled: "Canceled", error: "Error" };
const name = (path) => String(path || "").split("/").pop();
</script>

<template>
  <el-dialog :model-value="open" @update:model-value="!running && emit('close')" :title="t('peer.downloadQueue')" width="560" align-center
    :show-close="!running" :close-on-click-modal="false" :close-on-press-escape="!running" data-test="download-dialog">
    <div class="queue-dialog">
      <p role="status">{{ summary }}</p>
      <p v-if="queueState">{{ t('cloudBrowser.uploadSkip') }}: {{ queueState.skippedFiles }} · {{ queueState.currentRelativePath }}</p>
      <p v-if="queueState?.error" role="alert">{{ queueState.error.message }}</p>
      <TransferConflictControls v-if="open && queueState?.conflict" :key="`${queueState.queueId}:${queueState.conflict.conflictId}`" direction="download" :conflict="queueState.conflict" :apply-remaining="applyRemaining" :resolving="resolving || canceling" overwrite-allowed
        @update:apply-remaining="emit('update:applyRemaining', $event)" @resolve="emit('resolve', $event)" @cancel="emit('cancel')" />
      <el-button v-if="nextOffset != null" :disabled="pageLoading" data-test="download-next" @click="emit('page', nextOffset)">{{ t('cloudBrowser.uploadNextPage') }}</el-button>
      <el-progress v-if="percentage != null" :percentage="percentage" :stroke-width="6" />
      <p v-if="transferred != null">{{ formatBytes(transferred) }}<template v-if="total != null"> / {{ formatBytes(total) }}</template></p>
      <ul class="transfer-queue download-queue">
        <li v-for="item in items" :key="item.transferId || item.remotePath || item.relativePath" :data-status="item.status">
          <div class="queue-name"><FileGlyph :entry="{ isDir: false, name: item.relativePath }" /><div>
            <strong>{{ name(item.relativePath) }}</strong><span>{{ item.relativePath }}<template v-if="item.size != null"> · {{ formatBytes(item.size) }}</template></span>
            <details v-if="item.error"><summary>{{ t('transferQueue.details') }}</summary><p>{{ item.error }}</p></details>
          </div></div>
          <el-tag :type="statusTagTone(item.status)" effect="plain" round size="small">{{ t(`peer.downloadStatus${statusKey[item.status] || 'Pending'}`) }}</el-tag>
          <el-button v-if="item.retryable && !running" data-test="retry-download" @click="emit('retry', item)">{{ t('cloudBrowser.retryDownload') }}</el-button>
        </li>
      </ul>
      <p v-if="note" class="queue-note">{{ note }}</p>
    </div>
    <template #footer>
      <el-button v-if="running" data-test="cancel-download" plain :disabled="canceling" @click="emit('cancel')">{{ t(canceling ? 'peer.cancelingDownload' : 'peer.cancelDownload') }}</el-button>
      <template v-else><el-button v-if="root || queueState?.queueId" plain data-test="reveal-download" @click="emit('reveal')">{{ t('peer.revealInFolder') }}</el-button>
        <el-button type="primary" data-test="close-download" @click="emit('close')">{{ t('common.close') }}</el-button></template>
    </template>
  </el-dialog>
</template>

<style scoped>
.queue-dialog { display: flex; flex-direction: column; gap: 10px; }
.queue-dialog p { margin: 0; font-size: 13px; overflow-wrap: anywhere; }
.transfer-queue { list-style: none; max-height: min(360px, 45vh); overflow-y: auto; overscroll-behavior: contain; margin: 0; padding: 0; border: 1px solid var(--app-border); border-radius: 10px; }
.transfer-queue li { display: flex; align-items: center; gap: 10px; padding: 9px 11px; }
.transfer-queue li + li { border-top: 1px solid var(--app-border); }
.queue-name { flex: 1; display: flex; align-items: center; gap: 9px; min-width: 0; }
.queue-name > div { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
.queue-name strong { font-size: 13px; overflow-wrap: anywhere; }
.queue-name span, .queue-note, details { font-size: 12px; color: var(--app-text-muted); overflow-wrap: anywhere; }
summary { cursor: pointer; }
@media (max-width: 540px) { .transfer-queue li { flex-wrap: wrap; } .queue-name { flex-basis: 100%; } }
</style>
