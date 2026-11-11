<script setup>
import { t } from "../lib/i18n";
defineProps({ error: { type: Object, default: null }, retryable: Boolean, busy: Boolean, recheck: Boolean });
const emit = defineEmits(["close", "retry"]);
</script>
<template>
  <el-dialog :model-value="!!error" @update:model-value="emit('close')" :title="t('transferQueue.operationFailed')" width="480" align-center :close-on-click-modal="false" data-test="operation-error-dialog">
    <div v-if="error" class="operation-error" role="alert">
      <p class="cloud-error__message">{{ error.message }}</p><span class="error-code">{{ error.code }}</span>
      <details v-if="error.detail" :key="error"><summary>{{ t('transferQueue.details') }}</summary><p>{{ error.detail }}</p></details>
    </div>
    <template #footer>
      <div class="operation-error__actions">
        <el-button @click="emit('close')">{{ t('common.close') }}</el-button>
        <el-button v-if="retryable" :data-test="recheck ? 'recheck' : 'retry-load'" type="primary" :disabled="busy" @click="emit('retry')">{{ t(recheck ? 'cloudBrowser.recheck' : 'cloudBrowser.retry') }}</el-button>
      </div>
    </template>
  </el-dialog>
</template>
<style scoped>
.operation-error__actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 12px; }
.operation-error__actions :deep(.el-button + .el-button) { margin-left: 0; }
.operation-error { display: flex; flex-direction: column; gap: 10px; overflow-wrap: anywhere; }
p { margin: 0; white-space: pre-wrap; }
.error-code, details { font-size: 12px; color: var(--app-text-muted); }
summary { cursor: pointer; margin-bottom: 8px; }
</style>
