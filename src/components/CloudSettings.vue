<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { ElMessageBox } from "element-plus";
import "element-plus/es/components/message-box/style/css";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { cloud, blankConnection, blankMapping } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";
import { writeRoute } from "../lib/navigation";

const app = useAppContext();
const data = ref({ connections: [], repositories: [], mappings: [], statuses: [], capabilities: { safeApply: false } });
const connection = ref(blankConnection());
const mapping = ref(blankMapping());
const selectedRepoConnectionId = ref("");
const protocol = ref("cas");
const appendMode = ref("encrypted");
const appendBusy = ref(false);
const appendState = ref("idle");
const candidates = ref([]);
const selectedCandidateKey = ref("");
const trustedFingerprint = ref("");
const acceptTofu = ref(false);
const mappingProtocol = ref("cas");
const appendRepositories = computed(() => data.value.appendRepositories || []);
const mappingRepositories = computed(() => mappingProtocol.value === "append_v2" ? appendRepositories.value.filter((repo) => repo.connectionId === mapping.value.connectionId).map((repo) => ({ id: repo.binding.repoId, name: repo.binding.repoId })) : data.value.repositories);
const mappingAppendRepository = computed(() => appendRepositories.value.find((repo) => repo.connectionId === mapping.value.connectionId && repo.binding.repoId === mapping.value.repositoryId));
const rootCandidates = computed(() => mappingAppendRepository.value?.binding.rootCandidates || []);
const selectedCandidate = computed(() => candidates.value.find((item) => item.key === selectedCandidateKey.value));
const canAppendJoin = computed(() => !appendBusy.value && !!selectedCandidate.value && !!password.value && (!!trustedFingerprint.value.trim() || acceptTofu.value));
function resetTrust() { trustedFingerprint.value = ""; acceptTofu.value = false; }
function resetDiscovery() { candidates.value = []; selectedCandidateKey.value = ""; appendState.value = "idle"; resetTrust(); }
watch([selectedRepoConnectionId, protocol], () => { resetDiscovery(); password.value = ""; passwordConfirmation.value = ""; });
watch(selectedCandidateKey, resetTrust);
watch([mappingProtocol, () => mapping.value.connectionId], () => { if (!editingMappingId.value) { mapping.value.repositoryId = ""; mapping.value.logicalPath = ""; } });
watch(() => mapping.value.repositoryId, () => { if (!editingMappingId.value && mappingProtocol.value === "append_v2") mapping.value.logicalPath = ""; });
async function discoverAppend() {
  if (!selectedRepoConnectionId.value || appendBusy.value) return;
  resetDiscovery(); appendBusy.value = true; appendState.value = "loading";
  try { candidates.value = await cloud.appendDiscover(String(selectedRepoConnectionId.value)); appendState.value = candidates.value.length ? "ready" : "empty"; }
  catch (error) { appendState.value = "failed"; fail(error); }
  finally { appendBusy.value = false; }
}
async function createAppend() {
  if (!selectedRepoConnectionId.value || appendBusy.value || !password.value) return;
  if (password.value !== passwordConfirmation.value) { app.setError("login.mismatch"); return; }
  appendBusy.value = true; appendState.value = "loading";
  try { await cloud.appendCreate({ connectionId: String(selectedRepoConnectionId.value), mode: appendMode.value, password: password.value, passwordConfirmation: passwordConfirmation.value }); clearSecrets(); await refresh(); appendState.value = "saved"; }
  catch (error) { appendState.value = "failed"; fail(error); }
  finally { appendBusy.value = false; }
}
async function joinAppend() {
  if (!canAppendJoin.value) return;
  const candidate = selectedCandidate.value;
  appendBusy.value = true; appendState.value = "loading";
  try { await cloud.appendJoin({ connectionId: String(selectedRepoConnectionId.value), anchorKey: candidate.key, anchorFingerprint: candidate.fingerprint, repositoryId: candidate.repo_id, mode: candidate.descriptor.mode, acceptTofu: acceptTofu.value, trustedFingerprint: trustedFingerprint.value.trim() || null, password: password.value }); clearSecrets(); resetTrust(); await refresh(); appendState.value = "saved"; }
  catch (error) { appendState.value = "failed"; fail(error); }
  finally { appendBusy.value = false; }
}
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
async function saveMapping() {
  const input = { connectionId: mapping.value.connectionId, repositoryId: mapping.value.repositoryId, localRoot: mapping.value.localRoot, logicalPath: mapping.value.logicalPath, schedule: { enabled: Boolean(mapping.value.schedule?.enabled), interval: Math.max(60, Number(mapping.value.schedule?.interval) || 900) } };
  if (mappingProtocol.value === "append_v2" && (!mappingAppendRepository.value || !input.logicalPath || (rootCandidates.value.length && !rootCandidates.value.includes(input.logicalPath)))) { app.setError("cloudAppend.rootRequired"); return; }
  try {
    if (mappingProtocol.value === "append_v2") await cloud.appendSaveMapping(editingMappingId.value, input, input.logicalPath);
    else await cloud.saveMapping(editingMappingId.value, input);
    mapping.value = blankMapping(); editingMappingId.value = null; await refresh();
  } catch (error) { fail(error); }
}
async function editMapping(item) { editingMappingId.value = item.id; mappingProtocol.value = appendRepositories.value.some((repo) => repo.connectionId === item.connectionId && repo.binding.repoId === item.repositoryId) ? "append_v2" : "cas"; mapping.value = { ...blankMapping(), ...item, schedule: { ...blankMapping().schedule, ...(item.schedule || {}) } }; }
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
    <p class="cloud-safety-hint">{{ t("cloudBrowser.settingsHint") }}</p>
    <div v-for="item in data.connections" :key="`browse-${item.id}`" class="cloud-item"><span>{{ item.name }}</span><el-button class="cloud-browse-entry" @click="writeRoute({ view: 'cloud', connectionId: String(item.id) })">{{ t('cloudBrowser.open') }}</el-button></div>
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
      <label class="settings-row"><span class="settings-row__label">{{ t('cloudAppend.protocol') }}</span><el-select v-model="protocol" class="repository-protocol" :disabled="appendBusy"><el-option value="cas" :label="t('cloudAppend.cas')" /><el-option value="append_v2" :label="t('cloudAppend.append')" /></el-select></label>
      <div v-if="protocol === 'append_v2'" class="settings-rows append-settings" :aria-busy="appendBusy">
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.connection') }}</span><el-select v-model="selectedRepoConnectionId" class="append-connection" :disabled="appendBusy"><el-option v-for="item in data.connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('cloudAppend.mode') }}</span><el-select v-model="appendMode" class="append-mode" :disabled="appendBusy"><el-option value="encrypted" :label="t('cloud.encrypted')" /><el-option value="authenticated_plain" :label="t('cloudAppend.plain')" /></el-select></label>
        <p class="cloud-safety-hint">{{ t(appendMode === 'encrypted' ? 'cloudAppend.encryptedHint' : 'cloudAppend.plainHint') }}</p>
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.password') }}</span><el-input v-model="password" class="append-password" type="password" autocomplete="new-password" :disabled="appendBusy" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.passwordConfirm') }}</span><el-input v-model="passwordConfirmation" class="append-confirmation" type="password" autocomplete="new-password" :disabled="appendBusy" /></label>
        <div class="settings-row"><el-button class="append-create" :loading="appendBusy" :disabled="appendBusy || !selectedRepoConnectionId || !password || password !== passwordConfirmation" @click="createAppend">{{ t('cloud.create') }}</el-button><el-button class="append-discover" :disabled="appendBusy || !selectedRepoConnectionId" @click="discoverAppend">{{ t('cloudAppend.discover') }}</el-button></div>
        <p class="append-state" role="status">{{ t(`cloudAppend.${appendState}`) }}</p>
        <label v-if="candidates.length" class="settings-row"><span class="settings-row__label">{{ t('cloudAppend.candidate') }}</span><el-select v-model="selectedCandidateKey" class="append-candidate" :placeholder="t('cloudAppend.select')" :disabled="appendBusy"><el-option v-for="item in candidates" :key="item.key" :value="item.key" :label="`${item.repo_id} · ${item.key} · ${item.descriptor.mode}`" /></el-select></label>
        <div v-if="selectedCandidate" class="append-trust">
          <p>{{ t('cloudAppend.fingerprint') }}: {{ selectedCandidate.fingerprint }}</p><p>{{ t('cloudAppend.mode') }}: {{ selectedCandidate.descriptor.mode === 'encrypted' ? t('cloud.encrypted') : t('cloudAppend.plainHint') }}</p>
          <label class="settings-row"><span class="settings-row__label">{{ t('cloudAppend.trustedFingerprint') }}</span><el-input v-model="trustedFingerprint" class="append-fingerprint" :disabled="appendBusy" /></label>
          <label class="settings-row"><el-switch v-model="acceptTofu" class="append-tofu" :disabled="appendBusy" /><span>{{ t('cloudAppend.tofu') }}</span></label>
          <el-button class="append-join" :disabled="!canAppendJoin" :loading="appendBusy" @click="joinAppend">{{ t('cloud.join') }}</el-button>
        </div>
      </div>
      <div v-if="protocol === 'cas'" class="settings-rows"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.connection") }}</span><el-select v-model="selectedRepoConnectionId"><el-option v-for="item in data.connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.repositoryName") }}</span><el-input v-model="connection.repositoryName" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.password") }}</span><el-input v-model="password" type="password" autocomplete="new-password" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.passwordConfirm") }}</span><el-input v-model="passwordConfirmation" type="password" autocomplete="new-password" /></label><div class="settings-row"><el-button :disabled="!selectedRepoConnectionId" @click="createRepository">{{ t("cloud.create") }}</el-button><el-button :disabled="!selectedRepoConnectionId" @click="joinRepository">{{ t("cloud.join") }}</el-button><el-switch v-model="connection.encrypted" /><span>{{ connection.encrypted ? t("cloud.encrypted") : t("cloud.plainWarning") }}</span></div></div>
      <div v-for="repo in data.repositories" :key="repo.id" class="cloud-item"><strong>{{ repo.name || repo.id }}</strong><span>{{ repo.mode === "encrypted" ? t("cloud.encrypted") : t("cloud.plain") }}</span></div>
      <div v-for="repo in appendRepositories" :key="`${repo.connectionId}-${repo.binding.repoId}`" class="cloud-item"><strong>{{ repo.binding.repoId }}</strong><span>{{ t('cloudAppend.append') }} · {{ repo.binding.appendMode === 'encrypted' ? t('cloud.encrypted') : t('cloudAppend.plain') }}</span></div>
    </div>
    <div class="settings-group"><h3 class="group-title">{{ t("cloud.mappings") }}</h3>
      <label class="settings-row"><span class="settings-row__label">{{ t('cloudAppend.protocol') }}</span><el-select v-model="mappingProtocol" class="mapping-protocol" :disabled="!!editingMappingId"><el-option value="cas" :label="t('cloudAppend.cas')" /><el-option value="append_v2" :label="t('cloudAppend.append')" /></el-select></label>
      <div class="settings-rows"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.connection") }}</span><el-select v-model="mapping.connectionId"><el-option v-for="item in data.connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.repository") }}</span><el-select v-model="mapping.repositoryId"><el-option v-for="item in mappingRepositories" :key="item.id" :value="String(item.id)" :label="item.name || item.id" /></el-select></label><div class="settings-row settings-row--wide"><span class="settings-row__label">{{ t("cloud.localRoot") }}</span><el-input v-model="mapping.localRoot" /><el-button @click="chooseRoot">{{ t("cloud.choose") }}</el-button></div><label class="settings-row"><span class="settings-row__label">{{ t("cloud.logicalPath") }}</span><el-select v-if="mappingProtocol === 'append_v2' && rootCandidates.length" v-model="mapping.logicalPath" class="append-root" :placeholder="t('cloudAppend.select')"><el-option v-for="root in rootCandidates" :key="root" :value="root" :label="root" /></el-select><el-input v-else v-model="mapping.logicalPath" :placeholder="mappingProtocol === 'append_v2' ? t('cloudAppend.newRoot') : ''" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.schedule") }}</span><el-switch v-model="mapping.schedule.enabled" /><input v-model.number="mapping.schedule.interval" type="number" min="60" step="60" /></label><div class="settings-row settings-row--actions"><el-button type="primary" @click="saveMapping">{{ editingMappingId ? t("cloud.update") : t("cloud.save") }}</el-button></div></div>
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
.cloud-safety-hint, .cloud-progress, .append-trust { overflow-wrap: anywhere; line-height: 1.6; }
.append-settings, .append-trust { min-width: 0; max-width: 100%; }
@media (max-width: 400px) { .cloud-settings .settings-row > :deep(.el-select), .cloud-settings .settings-row > :deep(.el-input) { width: 100%; flex: 1 1 100%; } }
@media (max-width: 480px) { .cloud-settings .settings-row { align-items: stretch; } .cloud-settings .settings-row__label { flex-basis: 100%; } .cloud-settings .settings-row input { max-width: 100%; min-width: 0; } }
@media (max-width: 400px) { .cloud-item { align-items: flex-start; } }
</style>
