<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { ElMessageBox } from "element-plus";
import { cloud, blankMapping } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const props = defineProps({ connectionId: { type: String, required: true }, focusId: { type: String, default: "" } });
const emit = defineEmits(["changed"]);
const app = useAppContext();
const data = ref({ repositories: [], appendRepositories: [], mappings: [], statuses: [] });
const createDialogOpen = ref(false);
function closeCreateDialog() { if (busy.value) return; createDialogOpen.value = false; resetNewDrive(); }
const mode = ref("encrypted");
const newDrive = ref({ localRoot: "", logicalPath: "", password: "", confirmation: "" });
const password = ref("");
const busy = ref(false);
const state = ref("idle");
const candidates = ref([]);
const selectedCandidateKey = ref("");
const trustedFingerprint = ref("");
const acceptTofu = ref(false);
const editingMappingId = ref(null);
const mapping = ref(blankMapping());
const mappingEditorOpen = ref(false);
const preview = ref(null);
const recovery = ref([]);
const recoveryMappingId = ref("");
let disposed = false;
let generation = 0;
let unlisten;
const repositories = computed(() => [
  ...(data.value.repositories || []).filter((r) => r.connectionId === props.connectionId).map((r, i) => ({ ...r, label: t("drive.number", { number: i + 1 }), append: false })),
  ...(data.value.appendRepositories || []).filter((r) => r.connectionId === props.connectionId).map((r, i) => ({ id: r.binding.repoId, label: t("drive.number", { number: (data.value.repositories || []).filter((r) => r.connectionId === props.connectionId).length + i + 1 }), append: true, binding: r.binding })),
]);
const selectedRepository = computed(() => repositories.value.find((r) => r.id === mapping.value.repositoryId && (!editingMappingId.value || !mapping.value.protocol || r.append === (mapping.value.protocol === "append_v2"))));
const roots = computed(() => selectedRepository.value?.binding?.rootCandidates || []);
const mappings = computed(() => (data.value.mappings || []).filter((m) => m.connectionId === props.connectionId));
const statuses = computed(() => (data.value.statuses || []).filter((s) => mappings.value.some((m) => m.id === s.mappingId)));
const candidate = computed(() => candidates.value.find((c) => c.key === selectedCandidateKey.value));
const canJoin = computed(() => !busy.value && !!candidate.value && !!password.value && (!!trustedFingerprint.value.trim() || acceptTofu.value));
const canCreate = computed(() => !busy.value && !!props.connectionId && !!newDrive.value.localRoot.trim() && !!newDrive.value.password && newDrive.value.password === newDrive.value.confirmation);
const phaseLabel = (phase) => phase ? t(`cloud.phase${phase[0].toUpperCase()}${phase.slice(1)}`) : "";
function fail(error) { app.handleFailure(error); }
function resetTrust() { trustedFingerprint.value = ""; acceptTofu.value = false; }
function resetSecrets() { password.value = ""; resetTrust(); }
function resetNewDrive() { newDrive.value = { localRoot: "", logicalPath: "", password: "", confirmation: "" }; }
// Remote folder defaults to the chosen local folder's name, which keeps the
// one-step create usable without asking non-technical users to name things twice.
function folderName(path) { const parts = String(path).split(/[\\/]+/).filter(Boolean); return parts.length ? parts[parts.length - 1] : t("drive.defaultFolder"); }
watch(selectedCandidateKey, resetTrust);
watch(() => mapping.value.repositoryId, () => { if (!editingMappingId.value) mapping.value.logicalPath = ""; });
async function refresh(scope = generation) { const result = await cloud.list(); if (!disposed && scope === generation) data.value = result; }
async function run(operation) {
  if (busy.value) return;
  const scope = generation;
  busy.value = true;
  try { await operation(scope); } catch (error) { if (!disposed && scope === generation) { state.value = "failed"; fail(error); } }
  finally { if (!disposed && scope === generation) busy.value = false; }
}
watch(() => props.connectionId, () => {
  generation++; busy.value = false; createDialogOpen.value = false; resetSecrets(); resetNewDrive(); candidates.value = []; selectedCandidateKey.value = "";
  mapping.value = { ...blankMapping(), connectionId: props.connectionId }; editingMappingId.value = null; mappingEditorOpen.value = false;
  preview.value = null; recovery.value = []; state.value = "idle";
  void run((scope) => refresh(scope));
}, { immediate: true });
async function create() {
  if (!canCreate.value) return;
  await run(async (scope) => {
    state.value = "loading";
    const logicalPath = newDrive.value.logicalPath.trim() || folderName(newDrive.value.localRoot);
    const redacted = await cloud.createSyncDrive({ connectionId: props.connectionId, mode: mode.value, password: newDrive.value.password, passwordConfirmation: newDrive.value.confirmation });
    if (scope !== generation || disposed) return;
    // Bind the first local folder in the same step so a new drive always has
    // something to sync instead of looking like a no-op. The drive already
    // exists at this point, so surface a binding failure without leaving the
    // form filled (retrying create would otherwise duplicate the drive).
    let bindingError = null;
    try {
      await cloud.appendSaveMapping(null, { connectionId: props.connectionId, repositoryId: redacted.binding.repoId, localRoot: newDrive.value.localRoot, logicalPath, schedule: { enabled: false, interval: 900 } }, logicalPath);
    } catch (error) {
      bindingError = error;
    }
    if (scope !== generation || disposed) return;
    resetNewDrive(); createDialogOpen.value = false; await refresh(scope);
    if (scope !== generation || disposed) return;
    emit("changed");
    if (bindingError) throw bindingError;
    state.value = "saved";
  });
}
async function discover() {
  await run(async (scope) => {
    candidates.value = []; selectedCandidateKey.value = ""; resetTrust(); state.value = "loading";
    const result = await cloud.appendDiscover(props.connectionId);
    if (scope !== generation || disposed) return;
    candidates.value = result; state.value = result.length ? "ready" : "empty";
  });
}
async function join() {
  if (!canJoin.value) return;
  const item = candidate.value;
  await run(async (scope) => {
    state.value = "loading";
    await cloud.appendJoin({ connectionId: props.connectionId, anchorKey: item.key, anchorFingerprint: item.fingerprint, repositoryId: item.repo_id, mode: item.descriptor.mode, acceptTofu: acceptTofu.value, trustedFingerprint: trustedFingerprint.value.trim() || null, password: password.value });
    if (scope !== generation || disposed) return;
    resetSecrets(); await refresh(scope);
    if (scope !== generation || disposed) return;
    state.value = "saved"; emit("changed");
  });
}
function editMapping(item) { editingMappingId.value = item.id; mapping.value = { ...blankMapping(), ...item, schedule: { ...blankMapping().schedule, ...item.schedule } }; mappingEditorOpen.value = true; preview.value = null; }
function newMapping() { editingMappingId.value = null; mapping.value = { ...blankMapping(), connectionId: props.connectionId }; mappingEditorOpen.value = true; }
async function chooseNewRoot() {
  try {
    const value = await openDialog({ directory: true, multiple: false });
    if (!disposed && typeof value === "string") {
      newDrive.value.localRoot = value;
      if (!newDrive.value.logicalPath.trim()) newDrive.value.logicalPath = folderName(value);
    }
  } catch (error) { fail(error); }
}
async function chooseRoot() { try { const value = await openDialog({ directory: true, multiple: false }); if (!disposed && typeof value === "string") mapping.value.localRoot = value; } catch (error) { fail(error); } }
async function saveMapping() {
  if (!selectedRepository.value || !mapping.value.localRoot) { app.setError("drive.mappingRequired"); return; }
  const isAppend = selectedRepository.value.append;
  if (isAppend && (!mapping.value.logicalPath || (roots.value.length && !roots.value.includes(mapping.value.logicalPath)))) { app.setError("cloudAppend.rootRequired"); return; }
  const input = { ...mapping.value, connectionId: props.connectionId, schedule: { enabled: !!mapping.value.schedule.enabled, interval: Math.max(60, Number(mapping.value.schedule.interval) || 900) } };
  await run(async (scope) => {
    // Configuration, not a user-selectable protocol, determines the command.
    if (isAppend) await cloud.appendSaveMapping(editingMappingId.value, input, input.logicalPath);
    else await cloud.saveMapping(editingMappingId.value, input);
    if (scope !== generation || disposed) return;
    editingMappingId.value = null; mapping.value = { ...blankMapping(), connectionId: props.connectionId }; mappingEditorOpen.value = false;
    await refresh(scope);
  });
}
async function deleteMapping(item) { if (!await confirmAction(t("cloud.deleteMappingConfirm"), t("drive.removeMapping"))) return; await run(async (scope) => { await cloud.deleteMapping(String(item.id)); if (scope !== generation || disposed) return; if (preview.value?.mappingId === item.id) preview.value = null; await refresh(scope); if (scope === generation && !disposed) emit("changed"); }); }
async function showPreview(item) { await run(async (scope) => { const result = await cloud.preview(String(item.id)); if (scope === generation && !disposed) preview.value = { mappingId: String(item.id), previewId: String(result.previewId), summary: result.summary }; }); }
async function applyPreview() { if (!preview.value?.previewId || !preview.value?.mappingId) return; const target = { ...preview.value }; await run(async (scope) => { await cloud.sync(target.mappingId, target.previewId); if (scope !== generation || disposed) return; preview.value = null; await refresh(scope); }); }
async function cancel(item) { await run(async (scope) => { await cloud.cancel(String(item.id)); await refresh(scope); }); }
async function loadRecovery(item) { await run(async (scope) => { const result = await cloud.recoveryRecords(String(item.id)); if (scope === generation && !disposed) { recoveryMappingId.value = String(item.id); recovery.value = result; } }); }
async function restore(record) {
  if (!record.relativePath || !recoveryMappingId.value) return;
  const scope = generation;
  let destination;
  try { const result = await ElMessageBox.prompt(t("cloud.destinationPrompt"), t("cloud.restore"), { inputValue: record.relativePath, closeOnClickModal: false, confirmButtonText: t("confirm.ok"), cancelButtonText: t("confirm.cancel") }); destination = result.value; }
  catch (error) { if (error !== "cancel" && error !== "close") fail(error); return; }
  if (!destination || scope !== generation || disposed || !await confirmAction(t("cloud.restoreConfirm"), t("cloud.restore"))) return;
  await run(async () => { await cloud.restore(recoveryMappingId.value, String(record.id), destination); const result = await cloud.recoveryRecords(recoveryMappingId.value); if (scope === generation && !disposed) recovery.value = result; });
}
onMounted(async () => {
  try {
    const cleanup = await listen("app://cloud-progress", (event) => {
      if (disposed || !mappings.value.some((m) => m.id === event.payload.mappingId)) return;
      data.value.statuses = [...(data.value.statuses || []).filter((s) => s.mappingId !== event.payload.mappingId), event.payload];
    });
    if (disposed) await cleanup(); else unlisten = cleanup;
  } catch (error) { if (!disposed) fail(error); }
});
onBeforeUnmount(() => { disposed = true; generation++; if (unlisten) Promise.resolve().then(unlisten).catch(fail); });
</script>

<template>
  <section class="sync-drives" :aria-label="t('drive.manage')">
    <h3>{{ t('drive.manage') }}</h3><p class="hint">{{ t('cloud.safetyHint') }}</p>

    <!-- 主流程：一条路径完成「建盘 + 绑定首个本地文件夹」，其余收进高级。 -->
    <el-button class="open-create-drive" type="primary" :disabled="busy" @click="createDialogOpen = true">{{ t('drive.createTitle') }}</el-button>
    <p class="append-state" role="status">{{ t(`drive.${state}`) }}</p>
    <el-dialog :model-value="createDialogOpen" @update:model-value="closeCreateDialog" :title="t('drive.createTitle')" width="560" align-center
      :show-close="!busy" :close-on-click-modal="false" :close-on-press-escape="!busy" data-test="create-drive-dialog">
    <form class="drive-create" @submit.prevent="create">
      <label class="settings-row"><span>{{ t('drive.localFolder') }}</span>
        <el-input v-model="newDrive.localRoot" class="new-drive-local" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" />
        <el-button class="choose-new-root" :disabled="busy" @click="chooseNewRoot">{{ t('cloud.choose') }}</el-button>
      </label>
      <label class="settings-row"><span>{{ t('drive.remoteFolder') }}</span>
        <el-input v-model="newDrive.logicalPath" class="new-drive-folder" :placeholder="t('drive.folderDefaultHint')" :disabled="busy" />
      </label>
      <label class="settings-row"><span>{{ t('cloud.password') }}</span>
        <el-input v-model="newDrive.password" class="append-password" type="password" autocomplete="new-password" :disabled="busy" />
      </label>
      <label class="settings-row"><span>{{ t('cloud.passwordConfirm') }}</span>
        <el-input v-model="newDrive.confirmation" class="append-confirmation" type="password" autocomplete="new-password" :disabled="busy" />
      </label>
      <p class="hint">{{ t('drive.passwordHint') }}</p>
      <details class="create-advanced">
        <summary>{{ t('drive.createAdvanced') }}</summary>
        <label class="settings-row"><span>{{ t('cloudAppend.mode') }}</span><el-select v-model="mode" class="append-mode" :disabled="busy"><el-option value="encrypted" :label="t('cloud.encrypted')" /><el-option value="authenticated_plain" :label="t('cloudAppend.plain')" /></el-select></label>
        <p>{{ t(mode === 'encrypted' ? 'cloudAppend.encryptedHint' : 'cloudAppend.plainHint') }}</p>
      </details>
    </form>
    <template #footer>
      <el-button class="cancel-create-drive" :disabled="busy" @click="closeCreateDialog">{{ t('common.cancel') }}</el-button>
      <el-button class="append-create" type="primary" :disabled="!canCreate" :loading="busy" @click="create">{{ t('drive.createAndBind') }}</el-button>
    </template>
    </el-dialog>

    <!-- 高级：连接别人已经建好的同步盘。默认收起，避免干扰主流程。 -->
    <details class="drive-connect">
      <summary>{{ t('drive.connectExisting') }}</summary>
      <div class="settings-rows append-settings" :aria-busy="busy">
        <p class="hint">{{ t('drive.connectPasswordHint') }}</p>
        <label class="settings-row"><span>{{ t('cloud.password') }}</span><el-input v-model="password" class="append-join-password" type="password" autocomplete="current-password" :disabled="busy" /></label>
        <el-button class="append-discover" :disabled="busy || !connectionId" @click="discover">{{ t('drive.discover') }}</el-button>
        <label v-if="candidates.length" class="settings-row"><span>{{ t('drive.select') }}</span><el-select v-model="selectedCandidateKey" class="append-candidate" :disabled="busy"><el-option v-for="(item, index) in candidates" :key="item.key" :value="item.key" :label="t('drive.number', { number: index + 1 })" /></el-select></label>
        <div v-if="candidate" class="append-trust">
          <p>{{ candidate.descriptor.mode === 'encrypted' ? t('cloud.encrypted') : t('cloudAppend.plain') }}</p>
          <details class="advanced-security"><summary>{{ t('drive.advancedSecurity') }}</summary>
          <p>{{ t('drive.fingerprint') }}: {{ candidate.fingerprint }}</p>
          <label class="settings-row"><span>{{ t('cloudAppend.trustedFingerprint') }}</span><el-input v-model="trustedFingerprint" class="append-fingerprint" :disabled="busy" /></label>
          <label class="settings-row"><el-switch v-model="acceptTofu" class="append-tofu" :disabled="busy" /><span>{{ t('drive.tofu') }}</span></label>
          </details>
          <el-button class="append-join" :disabled="!canJoin" @click="join">{{ t('drive.connect') }}</el-button>
        </div>
      </div>
    </details>

    <ul class="drive-list"><li v-for="repo in repositories" :key="repo.id" :class="{ 'is-active': repo.id === focusId }">{{ repo.label }} · {{ repo.binding?.appendMode === 'authenticated_plain' || repo.mode === 'plain' ? t('cloudAppend.plain') : t('cloud.encrypted') }}</li></ul>

    <h4>{{ t('drive.localFolders') }}</h4>
    <el-button class="new-mapping" :disabled="busy" @click="newMapping">{{ t('drive.addFolder') }}</el-button>
    <div v-if="mappingEditorOpen" class="settings-rows mapping-editor">
      <label class="settings-row"><span>{{ t('drive.select') }}</span><el-select v-model="mapping.repositoryId" class="mapping-repository" :disabled="busy || !!editingMappingId"><el-option v-for="repo in repositories" :key="repo.id" :value="repo.id" :label="repo.label" /></el-select></label>
      <label class="settings-row"><span>{{ t('cloud.localRoot') }}</span><el-input v-model="mapping.localRoot" class="mapping-local" :disabled="busy" /><el-button class="choose-mapping-root" :disabled="busy" @click="chooseRoot">{{ t('cloud.choose') }}</el-button></label>
      <label class="settings-row"><span>{{ t('drive.folder') }}</span><el-select v-if="selectedRepository?.append && roots.length" v-model="mapping.logicalPath" class="append-root" :disabled="busy"><el-option v-for="(root, index) in roots" :key="root" :value="root" :label="t('drive.folderNumber', { number: index + 1, name: root })" /></el-select><el-input v-else v-model="mapping.logicalPath" class="mapping-remote" :placeholder="t('drive.newFolder')" :disabled="busy" /></label>
      <p>{{ t('drive.folderHint') }}</p>
      <!-- 新映射的自动同步由后端在首次保存时保持关闭，仅在编辑现有映射时提供开关。 -->
      <label v-if="editingMappingId" class="settings-row"><span>{{ t('cloud.schedule') }}</span><el-switch v-model="mapping.schedule.enabled" :disabled="busy" /><input v-model.number="mapping.schedule.interval" :disabled="busy" type="number" min="60" step="60" /></label>
      <el-button class="mapping-save" :disabled="busy" @click="saveMapping">{{ editingMappingId ? t('cloud.update') : t('cloud.save') }}</el-button>
    </div>
    <div v-for="item in mappings" :key="item.id" class="cloud-item mapping-item"><span>{{ item.localRoot }} → {{ item.logicalPath || '/' }}</span><el-button :disabled="busy" @click="editMapping(item)">{{ t('cloud.edit') }}</el-button><el-button :disabled="busy" @click="deleteMapping(item)">{{ t('drive.removeMapping') }}</el-button><el-button :disabled="busy" @click="showPreview(item)">{{ t('cloud.preview') }}</el-button><el-button :disabled="busy" @click="cancel(item)">{{ t('cloud.cancel') }}</el-button><el-button :disabled="busy" @click="loadRecovery(item)">{{ t('cloud.recovery') }}</el-button></div>
    <div v-if="preview" class="cloud-preview"><p>{{ t('cloud.confirmPreview') }}</p><p>{{ t('cloud.previewUploads') }}: {{ preview.summary?.uploads ?? 0 }} · {{ t('cloud.previewDownloads') }}: {{ preview.summary?.downloads ?? 0 }} · {{ t('cloud.previewDeletes') }}: {{ preview.summary?.deletes ?? 0 }} · {{ t('cloud.previewConflicts') }}: {{ preview.summary?.conflicts ?? 0 }}</p><pre v-if="typeof preview.summary === 'string'">{{ preview.summary }}</pre><el-button :disabled="busy" @click="applyPreview">{{ t('cloud.sync') }}</el-button></div>
    <ul v-if="recovery.length"><li v-for="record in recovery" :key="record.id">{{ record.relativePath }} · {{ record.phase }}<el-button :disabled="busy" @click="restore(record)">{{ t('cloud.restore') }}</el-button></li></ul>
    <p v-for="status in statuses" :key="status.mappingId" class="cloud-progress">{{ phaseLabel(status.phase) }} {{ status.detail }}</p>
  </section>
</template>

<style scoped>
.drive-list { padding: 0; list-style: none; }
.drive-list li { padding: 8px; border-radius: 6px; }
.drive-list .is-active { background: var(--app-surface-2); box-shadow: inset 3px 0 var(--el-color-primary); }
.sync-drives { min-width: 0; overflow-wrap: anywhere; }
.drive-create { padding: 12px; margin: .6rem 0; border-radius: 10px; background: var(--app-surface-2); }
.drive-create h4 { margin: 0 0 .4rem; }
.settings-row, .cloud-item { display: flex; flex-wrap: wrap; gap: .5rem; align-items: center; margin: .6rem 0; }
.sync-drives :deep(.el-input), .sync-drives :deep(.el-select) { min-width: 0; max-width: 100%; }
.cloud-preview pre { white-space: pre-wrap; }
summary { cursor: pointer; padding: .7rem 0; }
</style>