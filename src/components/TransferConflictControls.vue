<script setup>
import { t } from "../lib/i18n";
defineProps({ conflict: { type: Object, required: true }, applyRemaining: Boolean, resolving: Boolean, overwriteAllowed: Boolean, direction: { type: String, default: "upload" } });
const emit = defineEmits(["resolve", "cancel", "update:applyRemaining"]);
// 对象键与 ETag 原样来自后端，展示时去掉包裹的引号，避免用户当成值的一部分。
const plain = (value) => String(value ?? "").replaceAll('"', "");
</script>
<template>
  <el-dialog :model-value="true" append-to-body :title="t('cloudBrowser.transferConflict')" width="480" align-center
    :show-close="false" :close-on-click-modal="false" :close-on-press-escape="false" :data-test="`${direction}-conflict-dialog`">
    <div :data-test="`${direction}-conflict`" class="conflict-body">
      <!-- 标题已说明“文件已存在”，正文只讲后果，不再重复标题文案。 -->
      <p class="conflict-summary">{{ t(direction === 'download' ? 'cloudBrowser.conflictBodyDownload' : 'cloudBrowser.conflictBodyUpload') }}</p>
      <div class="conflict-path">
        <span class="conflict-path__label">{{ t('cloudBrowser.conflictPathLabel') }}</span>
        <span class="conflict-path__value" :title="conflict.relativePath">{{ conflict.relativePath }}</span>
      </div>

      <details v-if="conflict.key != null || conflict.etag != null" class="conflict-details">
        <summary>{{ t('transferQueue.details') }}</summary>
        <p v-if="conflict.key != null" class="mono">{{ t('cloudBrowser.conflictRemotePath') }}{{ plain(conflict.key) }}</p>
        <p v-if="conflict.etag != null" class="mono">ETag: {{ plain(conflict.etag) }}</p>
      </details>

      <label class="conflict-apply"><input :checked="applyRemaining" @change="emit('update:applyRemaining', $event.target.checked)" :data-test="`${direction}-apply-remaining`" type="checkbox"><span>{{ t('cloudBrowser.uploadApplyRemaining') }}</span></label>

      <div class="conflict-actions">
        <el-button class="conflict-actions__cancel" :data-test="`${direction}-conflict-cancel`" :disabled="resolving" @click="emit('cancel')">{{ t('common.cancel') }}</el-button>
        <el-button :data-test="`${direction}-skip`" :disabled="resolving" @click="emit('resolve', 'skip')">{{ t('cloudBrowser.uploadSkip') }}</el-button>
        <el-button type="primary" :data-test="`${direction}-overwrite`" :disabled="resolving || !overwriteAllowed" @click="emit('resolve', 'overwrite')">{{ t('cloudBrowser.uploadOverwriteAction') }}</el-button>
      </div>
    </div>
  </el-dialog>
</template>

<style scoped>
.conflict-body { display: flex; flex-direction: column; gap: 12px; }
.conflict-summary { margin: 0; font-size: 13px; line-height: 1.6; color: var(--app-text); overflow-wrap: anywhere; }

/* 冲突路径是决策对象本身：等宽小块展示，与次要说明拉开层级。 */
.conflict-path { display: flex; align-items: baseline; gap: 8px; }
.conflict-path__label { flex: none; font-size: 12px; color: var(--app-text-muted); }
.conflict-path__value { flex: 1; min-width: 0; padding: 2px 6px; border-radius: 4px; background: var(--app-surface-2); font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace; font-size: 12px; line-height: 18px; color: var(--app-text); overflow-wrap: anywhere; }

/* 对象键、ETag 属于排查用的技术细节，默认折叠、弱化展示，避免和决策抢注意力。 */
.conflict-details { font-size: 12px; color: var(--app-text-muted); }
.conflict-details summary { cursor: pointer; }
.conflict-details p { margin: 4px 0 0; }
.mono { font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace; font-size: 11.5px; color: var(--app-text-muted); overflow-wrap: anywhere; }

/* 整行都是点击区，命中高度不低于 28px。 */
.conflict-apply { display: flex; align-items: center; gap: 8px; min-height: 28px; font-size: 13px; line-height: 1.5; color: var(--app-text-secondary); cursor: pointer; }
.conflict-apply input { flex: none; width: 16px; height: 16px; margin: 0; accent-color: var(--el-color-primary); }

.conflict-actions { display: flex; align-items: center; gap: 8px; }
.conflict-actions__cancel { margin-right: auto; }
.conflict-actions :deep(.el-button + .el-button) { margin-left: 0; }
</style>
