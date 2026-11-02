<script setup>
import { t } from "../lib/i18n";
defineProps({ conflict: { type: Object, required: true }, applyRemaining: Boolean, resolving: Boolean, overwriteAllowed: Boolean, direction: { type: String, default: "upload" } });
const emit = defineEmits(["resolve", "cancel", "update:applyRemaining"]);
</script>
<template>
  <el-dialog :model-value="true" append-to-body :title="t('cloudBrowser.transferConflict')" width="480" align-center
    :show-close="false" :close-on-click-modal="false" :close-on-press-escape="false" :data-test="`${direction}-conflict-dialog`">
  <div :data-test="`${direction}-conflict`">
    <p>{{ t('cloudBrowser.transferConflict') }}: {{ conflict.relativePath }}</p>
    <p v-if="conflict.key != null" class="mono">{{ JSON.stringify(conflict.key) }}</p>
    <p v-if="conflict.etag != null" class="mono">ETag: {{ conflict.etag }}</p>
    <label><input :checked="applyRemaining" @change="emit('update:applyRemaining', $event.target.checked)" :data-test="`${direction}-apply-remaining`" type="checkbox"> {{ t('cloudBrowser.uploadApplyRemaining') }}</label>
    <div>
      <el-button :data-test="`${direction}-skip`" :disabled="resolving" @click="emit('resolve', 'skip')">{{ t('cloudBrowser.uploadSkip') }}</el-button>
      <el-button :data-test="`${direction}-overwrite`" :disabled="resolving || !overwriteAllowed" @click="emit('resolve', 'overwrite')">{{ t('cloudBrowser.uploadOverwriteAction') }}</el-button>
      <el-button :data-test="`${direction}-conflict-cancel`" :disabled="resolving" @click="emit('cancel')">{{ t('common.cancel') }}</el-button>
    </div>
  </div>
  </el-dialog>
</template>
