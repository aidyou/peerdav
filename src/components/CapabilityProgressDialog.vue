<script setup>
import { computed, onMounted, onUnmounted, ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { t } from "../lib/i18n";
const entries = ref({});
const hidden = ref(false);
const active = computed(() => Object.values(entries.value));
let disposed = false;
let unlisten;
onMounted(async () => {
  try {
    const stop = await listen("app://cloud-capability-progress", ({ payload }) => {
      if (disposed || !payload?.scope || !["cas", "move", "delete"].includes(payload.capability)) return;
      const key = `${payload.scope}:${payload.capability}`;
      if (["completed", "failed"].includes(payload.stage)) {
        const next = { ...entries.value }; delete next[key]; entries.value = next;
        if (!active.value.length) hidden.value = false;
      } else if (["checking", "cleaning"].includes(payload.stage)) {
        entries.value = { ...entries.value, [key]: payload };
      }
    });
    if (disposed) stop(); else unlisten = stop;
  } catch (error) { console.error("Capability progress subscription failed", error); }
});
onUnmounted(() => { disposed = true; unlisten?.(); });
</script>
<template>
  <el-dialog :model-value="active.length > 0 && !hidden" @update:model-value="hidden = true" :title="t('cloudProbe.title')" width="480" align-center append-to-body :close-on-click-modal="false" data-test="capability-progress">
    <div role="status" aria-live="polite" aria-busy="true">
      <p>{{ t('cloudProbe.hint') }}</p>
      <ul>
        <li v-for="entry in active" :key="`${entry.scope}:${entry.capability}`">
          <strong>{{ entry.connectionName }}</strong>
          <p>{{ t(`cloudProbe.${entry.capability}`) }} · {{ t(`cloudProbe.${entry.stage}`) }}</p>
        </li>
      </ul>
      <progress :aria-label="t('cloudProbe.title')" />
    </div>
    <template #footer><el-button @click="hidden = true">{{ t('cloudProbe.background') }}</el-button></template>
  </el-dialog>
</template>
<style scoped>
p { overflow-wrap: anywhere; }
ul { padding-inline-start: 20px; }
progress { width: 100%; accent-color: auto; }
</style>
