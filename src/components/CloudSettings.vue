<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { ElMessageBox } from "element-plus";
import "element-plus/es/components/message-box/style/css";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { cloud, blankConnection, blankMapping } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const app = useAppContext();
const data = ref({ connections: [], repositories: [], mappings: [], statuses: [], capabilities: { safeApply: false } });
const connection = ref(blankConnection());
const mapping = ref(blankMapping());
const selectedRepoConnectionId = ref("");
const password = ref("");
const passwordConfirmation = ref("");
const loading = ref(false);
const preview = ref(null);
const previewMappingId = ref("");
const recovery = ref([]);
const recoveryMappingId = ref("");
const progress = ref(null);
const editingMappingId = ref(null);
let unlisten;
const phaseLabel = (phase) => phase ? t(`cloud.phase${phase[0].toUpperCase()}${phase.slice(1)}`) : "";
const selectedStatus = computed(() => data.value.statuses || []);
function fail(error) { app.handleFailure(error); }
async function refresh() { try { data.value = await cloud.list(); if (!selectedRepoConnectionId.value && data.value.connections.length) selectedRepoConnectionId.value = String(data.value.connections[0].id); } catch (error) { fail(error); } }
function clearSecrets() { connection.value.secretKey = ""; connection.value.accessKey = ""; connection.value.sessionToken = ""; password.value = ""; passwordConfirmation.value = ""; if (connection.value.proxy?.mode === "custom") connection.value.proxy = { ...connection.value.proxy, username: "", password: "" }; }
function selectConnection(item) { connection.value = { ...blankConnection(), ...item, accessKey: "", secretKey: "", sessionToken: "", proxy: item.proxy || { mode: "direct" } }; }
async function saveConnection() { loading.value = true; try { await cloud.saveConnection({ ...connection.value, accessKey: connection.value.accessKey || undefined, secretKey: connection.value.secretKey || undefined, sessionToken: connection.value.sessionToken || undefined, schedule: connection.value.schedule }); clearSecrets(); connection.value.clearCredentials = false; connection.value.clearSessionToken = false; connection.value.clearProxyAuth = false; await refresh(); const saved = data.value.connections.find((item) => item.name === connection.value.name); if (saved) { connection.value.id = saved.id; selectedRepoConnectionId.value = String(saved.id); } app.setNotice("cloud.saved"); } catch (error) { fail(error); } finally { loading.value = false; } }
async function testConnection(item) { try { await cloud.testConnection(String(item.id)); await refresh(); } catch (error) { fail(error); } }
async function deleteConnection(item) { if (!item?.id || !await confirmAction(t("cloud.deleteConnectionConfirm"), t("cloud.delete"))) return; try { await cloud.deleteConnection(String(item.id)); await refresh(); } catch (error) { fail(error); } }
async function createRepository() { const connectionId = String(selectedRepoConnectionId.value || ""); if (!connectionId) { app.setError("cloud.connectionRequired"); return; } if (password.value !== passwordConfirmation.value) { app.setError("login.mismatch"); return; } try { await cloud.createRepository({ connectionId, name: connection.value.repositoryName || "", encrypted: connection.value.encrypted !== false, password: password.value, passwordConfirmation: passwordConfirmation.value }); clearSecrets(); await refresh(); } catch (error) { fail(error); } }
async function joinRepository() { const connectionId = String(selectedRepoConnectionId.value || ""); if (!connectionId) { app.setError("cloud.connectionRequired"); return; } try { await cloud.joinRepository({ connectionId, repositoryId: String(connection.value.repositoryId), password: password.value }); clearSecrets(); await refresh(); } catch (error) { fail(error); } }
async function saveMapping() { try { await cloud.saveMapping(editingMappingId.value, { connectionId: mapping.value.connectionId, repositoryId: mapping.value.repositoryId, localRoot: mapping.value.localRoot, logicalPath: mapping.value.logicalPath, schedule: { enabled: Boolean(mapping.value.schedule?.enabled), interval: Math.max(60, Number(mapping.value.schedule?.interval) || 900) } }); mapping.value = blankMapping(); editingMappingId.value = null; await refresh(); } catch (error) { fail(error); } }
async function editMapping(item) { editingMappingId.value = item.id; mapping.value = { ...blankMapping(), ...item, schedule: { ...blankMapping().schedule, ...(item.schedule || {}) } }; }
async function deleteMapping(item) { if (!await confirmAction(t("cloud.deleteMappingConfirm"), t("cloud.delete"))) return; try { await cloud.deleteMapping(String(item.id)); if (previewMappingId.value === item.id) preview.value = null; await refresh(); } catch (error) { fail(error); } }
async function chooseRoot() { try { const selected = await openDialog({ directory: true, multiple: false }); if (typeof selected === "string") mapping.value.localRoot = selected; } catch (error) { fail(error); } }
async function showPreview(item) { try { const result = await cloud.preview(String(item.id)); preview.value = { mappingId: String(item.id), previewId: String(result.previewId), summary: result.summary }; previewMappingId.value = String(item.id); } catch (error) { fail(error); } }
async function applyPreview() { if (!preview.value?.previewId || !preview.value?.mappingId) return; try { await cloud.sync(preview.value.mappingId, preview.value.previewId); preview.value = null; await refresh(); } catch (error) { fail(error); } }
async function cancel(item) { try { await cloud.cancel(String(item.id)); await refresh(); } catch (error) { fail(error); } }
async function loadRecovery(item) { try { recoveryMappingId.value = String(item.id); recovery.value = await cloud.recoveryRecords(String(item.id)); } catch (error) { fail(error); } }
async function restore(record) { if (!record.relativePath || !recoveryMappingId.value) return; let destination; try { const result = await ElMessageBox.prompt(t("cloud.destinationPrompt"), t("cloud.restore"), { inputValue: record.relativePath, closeOnClickModal: false, confirmButtonText: t("confirm.ok"), cancelButtonText: t("confirm.cancel") }); destination = result.value; } catch (error) { if (error !== "cancel" && error !== "close") fail(error); return; } if (!destination || !await confirmAction(t("cloud.restoreConfirm"), t("cloud.restore"))) return; try { await cloud.restore(recoveryMappingId.value, String(record.id), destination); await loadRecovery({ id: recoveryMappingId.value }); } catch (error) { fail(error); } }
let disposed = false;
onMounted(async () => { await refresh(); if (disposed) return; try { const cleanup = await listen("app://cloud-progress", (event) => { if (!disposed) { progress.value = event.payload; const statusIndex = data.value.statuses.findIndex((status) => status.mappingId === event.payload.mappingId); if (statusIndex < 0) data.value.statuses.push(event.payload); else data.value.statuses[statusIndex] = event.payload; } }); if (disposed) await cleanup(); else unlisten = cleanup; } catch (error) { if (!disposed) fail(error); } });
onBeforeUnmount(() => { disposed = true; if (unlisten) { const cleanup = unlisten; unlisten = null; Promise.resolve().then(cleanup).catch((error) => fail(error)); } });
</script>
<template>
  <div class="cloud-settings">
    <p class="cloud-safety-hint">{{ t("cloud.safetyHint") }}</p>
    <div class="settings-group"><h3 class="group-title">{{ t("cloud.connections") }}</h3>
      <div class="settings-rows">
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.name") }}</span><el-input v-model="connection.name" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.endpoint") }}</span><el-input v-model="connection.endpoint" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.bucket") }}</span><el-input v-model="connection.bucket" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.region") }}</span><el-input v-model="connection.region" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.prefix") }}</span><el-input v-model="connection.prefix" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.accessKey") }}</span><el-input v-model="connection.accessKey" autocomplete="off" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.secretKey") }}</span><el-input v-model="connection.secretKey" type="password" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.sessionToken") }}</span><el-input v-model="connection.sessionToken" type="password" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxy") }}</span><el-select v-model="connection.proxy.mode"><el-option value="direct" :label="t('cloud.proxyDirect')" /><el-option value="environment" :label="t('cloud.proxyEnvironment')" /><el-option value="custom" :label="t('cloud.proxyCustom')" /></el-select></label>
        <template v-if="connection.proxy.mode === 'custom'"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyUrl") }}</span><el-input v-model="connection.proxy.url" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyUsername") }}</span><el-input v-model="connection.proxy.username" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyPassword") }}</span><el-input v-model="connection.proxy.password" type="password" /></label></template>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.allowInsecureHttp") }}</span><el-switch v-model="connection.allowInsecureHttp" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.pathStyle") }}</span><el-switch v-model="connection.pathStyle" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearCredentials") }}</span><el-switch v-model="connection.clearCredentials" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearSessionToken") }}</span><el-switch v-model="connection.clearSessionToken" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearProxyAuth") }}</span><el-switch v-model="connection.clearProxyAuth" /></label>
        <div class="settings-row settings-row--actions"><el-button type="primary" :loading="loading" @click="saveConnection">{{ t("cloud.save") }}</el-button><el-button @click="testConnection(connection)">{{ t("cloud.test") }}</el-button><el-button @click="deleteConnection(connection)">{{ t("cloud.delete") }}</el-button></div>
      </div>
      <div v-for="item in data.connections" :key="item.id" class="cloud-item"><span>{{ item.name }} · {{ item.id }}</span><span>{{ item.endpoint }} / {{ item.bucket }}</span><el-button size="small" @click="selectConnection(item)">{{ t("cloud.edit") }}</el-button><el-button size="small" @click="testConnection(item)">{{ t("cloud.test") }}</el-button><el-button size="small" @click="deleteConnection(item)">{{ t("cloud.delete") }}</el-button></div>
    </div>
    <div class="settings-group"><h3 class="group-title">{{ t("cloud.repositories") }}</h3>
      <div class="settings-rows"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.connection") }}</span><el-select v-model="selectedRepoConnectionId"><el-option v-for="item in data.connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.repositoryName") }}</span><el-input v-model="connection.repositoryName" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.password") }}</span><el-input v-model="password" type="password" autocomplete="new-password" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.passwordConfirm") }}</span><el-input v-model="passwordConfirmation" type="password" autocomplete="new-password" /></label><div class="settings-row"><el-button :disabled="!selectedRepoConnectionId" @click="createRepository">{{ t("cloud.create") }}</el-button><el-button :disabled="!selectedRepoConnectionId" @click="joinRepository">{{ t("cloud.join") }}</el-button><el-switch v-model="connection.encrypted" /><span>{{ connection.encrypted ? t("cloud.encrypted") : t("cloud.plainWarning") }}</span></div></div>
      <div v-for="repo in data.repositories" :key="repo.id" class="cloud-item"><strong>{{ repo.name || repo.id }}</strong><span>{{ repo.mode === "encrypted" ? t("cloud.encrypted") : t("cloud.plain") }}</span></div>
    </div>
    <div class="settings-group"><h3 class="group-title">{{ t("cloud.mappings") }}</h3>
      <div class="settings-rows"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.connection") }}</span><el-select v-model="mapping.connectionId"><el-option v-for="item in data.connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.repository") }}</span><el-select v-model="mapping.repositoryId"><el-option v-for="item in data.repositories" :key="item.id" :value="String(item.id)" :label="item.name || item.id" /></el-select></label><div class="settings-row settings-row--wide"><span class="settings-row__label">{{ t("cloud.localRoot") }}</span><el-input v-model="mapping.localRoot" /><el-button @click="chooseRoot">{{ t("cloud.choose") }}</el-button></div><label class="settings-row"><span class="settings-row__label">{{ t("cloud.logicalPath") }}</span><el-input v-model="mapping.logicalPath" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.schedule") }}</span><el-switch v-model="mapping.schedule.enabled" /><input v-model.number="mapping.schedule.interval" type="number" min="60" step="60" /></label><div class="settings-row settings-row--actions"><el-button type="primary" @click="saveMapping">{{ editingMappingId ? t("cloud.update") : t("cloud.save") }}</el-button></div></div>
      <div v-for="item in data.mappings" :key="item.id" class="cloud-item cloud-item--actions"><span>{{ item.localRoot }} → {{ item.logicalPath }}</span><el-button size="small" @click="editMapping(item)">{{ t("cloud.edit") }}</el-button><el-button size="small" @click="deleteMapping(item)">{{ t("cloud.delete") }}</el-button><el-button size="small" @click="showPreview(item)">{{ t("cloud.preview") }}</el-button><el-button size="small" @click="cancel(item)">{{ t("cloud.cancel") }}</el-button><el-button size="small" @click="loadRecovery(item)">{{ t("cloud.recovery") }}</el-button></div>
      <div v-if="preview" class="cloud-preview"><p>{{ t("cloud.confirmPreview") }}</p><p>{{ t("cloud.previewUploads") }}: {{ preview.summary?.uploads ?? 0 }} · {{ t("cloud.previewDownloads") }}: {{ preview.summary?.downloads ?? 0 }} · {{ t("cloud.previewDeletes") }}: {{ preview.summary?.deletes ?? 0 }} · {{ t("cloud.previewConflicts") }}: {{ preview.summary?.conflicts ?? 0 }}</p><pre v-if="typeof preview.summary === 'string'">{{ preview.summary }}</pre><el-button type="primary" @click="applyPreview">{{ t("cloud.sync") }}</el-button></div>
      <ul v-if="recovery.length" class="cloud-recovery"><li v-for="record in recovery" :key="record.id">{{ record.relativePath }} · {{ record.phase }} <el-button size="small" @click="restore(record)">{{ t("cloud.restore") }}</el-button></li></ul>
      <div v-for="status in selectedStatus" :key="status.mappingId" class="cloud-progress">{{ status.mappingId }} · {{ phaseLabel(status.phase) }} <span v-if="status.detail">{{ status.detail }}</span></div>
      <div v-if="progress" class="cloud-progress">{{ phaseLabel(progress.phase) }}</div>
    </div>
  </div>
</template>

<style scoped>
.cloud-settings { min-width: 0; }
.cloud-item { display: flex; flex-wrap: wrap; gap: 0.5rem; min-width: 0; overflow-wrap: anywhere; align-items: center; }
.cloud-item > span { min-width: 0; }
.cloud-settings .settings-row { min-width: 0; flex-wrap: wrap; gap: 0.5rem; }
.cloud-settings :deep(.el-input), .cloud-settings :deep(.el-select) { min-width: 0; max-width: 100%; }
.cloud-preview pre { white-space: pre-wrap; overflow-wrap: anywhere; }
.cloud-safety-hint, .cloud-progress { overflow-wrap: anywhere; line-height: 1.6; }
@media (max-width: 480px) { .cloud-settings .settings-row { align-items: stretch; } .cloud-settings .settings-row__label { flex-basis: 100%; } .cloud-settings .settings-row input { max-width: 100%; min-width: 0; } }
@media (max-width: 400px) { .cloud-item { align-items: flex-start; } }
</style>
