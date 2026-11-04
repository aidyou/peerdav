<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { ElMessageBox } from "element-plus";
import { cloud, cloudObjects, blankMapping, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const props = defineProps({ connectionId: { type: String, required: true }, focusId: { type: String, default: "" } });
const emit = defineEmits(["changed"]);
const app = useAppContext();
const data = ref({ repositories: [], appendRepositories: [], mappings: [], statuses: [], mirrorMappings: [] });
const createDialogOpen = ref(false);
const createTab = ref("create");
// 新建同步盘的内容模式：开 = 加密同步盘（append），关 = 明文镜像同步。
const encrypted = ref(true);
function closeCreateDialog() { if (busy.value) return; createDialogOpen.value = false; resetNewDrive(); resetLink(); resetMirror(); }
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
// 链接已有同步盘：join 之后需要绑定本地目录；多根目录时选择，空仓库时命名。
const linkForm = ref({ localRoot: "", selectedRoot: "", newRoot: "" });
const linkRoots = ref([]);
const joinedRepoId = ref("");
// 镜像同步：创建表单与运行状态。
const mirrorForm = ref({ localRoot: "", cloudPrefix: "" });
const mirrorBusy = ref({});
const mirrorStatus = ref({});
const mirrorResults = ref({});
// 云端目录选择器。
const pickerOpen = ref(false);
const pickerPrefix = ref("");
const pickerEntries = ref([]);
const pickerLoading = ref(false);
const pickerNewName = ref("");
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
const mirrorMappings = computed(() => (data.value.mirrorMappings || []).filter((m) => m.connectionId === props.connectionId));
const statuses = computed(() => (data.value.statuses || []).filter((s) => mappings.value.some((m) => m.id === s.mappingId)));
const candidate = computed(() => candidates.value.find((c) => c.key === selectedCandidateKey.value));
const canJoin = computed(() => !busy.value && !!candidate.value && !!password.value && (!!trustedFingerprint.value.trim() || acceptTofu.value) && !!linkForm.value.localRoot.trim());
const canCreate = computed(() => !busy.value && !!props.connectionId && !!newDrive.value.localRoot.trim() && !!newDrive.value.password && newDrive.value.password === newDrive.value.confirmation);
const canCreateMirror = computed(() => !busy.value && !!props.connectionId && !!mirrorForm.value.localRoot.trim());
const pickerBreadcrumbs = computed(() => {
  const crumbs = [{ name: "_peerdav_mirror", prefix: "" }];
  const segments = pickerPrefix.value.split("/").filter(Boolean);
  let path = "";
  for (const segment of segments) { path += `${segment}/`; crumbs.push({ name: segment, prefix: path }); }
  return crumbs;
});
const phaseLabel = (phase) => phase ? t(`cloud.phase${phase[0].toUpperCase()}${phase.slice(1)}`) : "";
function fail(error) { app.handleFailure(error); }
function resetTrust() { trustedFingerprint.value = ""; acceptTofu.value = false; }
function resetSecrets() { password.value = ""; resetTrust(); }
function resetNewDrive() { newDrive.value = { localRoot: "", logicalPath: "", password: "", confirmation: "" }; }
function resetLink() { linkForm.value = { localRoot: "", selectedRoot: "", newRoot: "" }; linkRoots.value = []; joinedRepoId.value = ""; candidates.value = []; selectedCandidateKey.value = ""; resetSecrets(); }
function resetMirror() { mirrorForm.value = { localRoot: "", cloudPrefix: "" }; }
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
  generation++; busy.value = false; createDialogOpen.value = false; resetSecrets(); resetNewDrive(); resetLink(); resetMirror();
  mapping.value = { ...blankMapping(), connectionId: props.connectionId }; editingMappingId.value = null; mappingEditorOpen.value = false;
  preview.value = null; recovery.value = []; state.value = "idle"; mirrorBusy.value = {}; mirrorStatus.value = {}; mirrorResults.value = {};
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
async function createMirror() {
  if (!canCreateMirror.value) return;
  await run(async (scope) => {
    state.value = "loading";
    await cloud.mirrorSaveMapping({ id: null, connectionId: props.connectionId, localRoot: mirrorForm.value.localRoot.trim(), cloudPrefix: mirrorForm.value.cloudPrefix.trim(), schedule: { enabled: false, interval: 900 } });
    if (scope !== generation || disposed) return;
    resetMirror(); createDialogOpen.value = false; await refresh(scope);
    if (scope !== generation || disposed) return;
    emit("changed");
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
    const redacted = await cloud.appendJoin({ connectionId: props.connectionId, anchorKey: item.key, anchorFingerprint: item.fingerprint, repositoryId: item.repo_id, mode: item.descriptor.mode, acceptTofu: acceptTofu.value, trustedFingerprint: trustedFingerprint.value.trim() || null, password: password.value });
    if (scope !== generation || disposed) return;
    await refresh(scope);
    if (scope !== generation || disposed) return;
    // 加入后立即绑定本地目录；根目录唯一时一步完成，多个或为空时交给用户选择/命名。
    joinedRepoId.value = redacted.binding.repoId;
    const repo = (data.value.appendRepositories || []).find((r) => r.binding.repoId === joinedRepoId.value);
    linkRoots.value = repo?.binding?.rootCandidates || [];
    if (linkRoots.value.length === 1) {
      await bindLogicalPath(scope, linkRoots.value[0]);
    } else {
      state.value = "ready";
    }
  });
}
// 绑定核心：join 的单根目录自动路径与用户在多根目录/空仓库后的确认都走这里。
// 不能再套一层 run()：join 已经持有 busy 锁，二次进入会被守卫吞掉。
async function bindLogicalPath(scope, logicalPath) {
  state.value = "loading";
  await cloud.appendSaveMapping(null, { connectionId: props.connectionId, repositoryId: joinedRepoId.value, localRoot: linkForm.value.localRoot, logicalPath, schedule: { enabled: false, interval: 900 } }, logicalPath);
  if (scope !== generation || disposed) return;
  resetSecrets(); resetLink(); createDialogOpen.value = false; await refresh(scope);
  if (scope !== generation || disposed) return;
  state.value = "saved"; emit("changed");
}
async function confirmBind() {
  const logicalPath = linkForm.value.selectedRoot || linkForm.value.newRoot.trim();
  if (!logicalPath || !joinedRepoId.value || !linkForm.value.localRoot.trim()) return;
  await run((scope) => bindLogicalPath(scope, logicalPath));
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
async function chooseMirrorRoot() {
  try {
    const value = await openDialog({ directory: true, multiple: false });
    if (!disposed && typeof value === "string") mirrorForm.value.localRoot = value;
  } catch (error) { fail(error); }
}
async function chooseLinkRoot() {
  try {
    const value = await openDialog({ directory: true, multiple: false });
    if (!disposed && typeof value === "string") linkForm.value.localRoot = value;
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
async function deleteMapping(item) { if (!await confirmAction(t("cloud.deleteMappingConfirm"), t("drive.removeMapping"), { danger: true, confirmText: t("common.delete") })) return; await run(async (scope) => { await cloud.deleteMapping(String(item.id)); if (scope !== generation || disposed) return; if (preview.value?.mappingId === item.id) preview.value = null; await refresh(scope); if (scope === generation && !disposed) emit("changed"); }); }
async function deleteMirror(item) {
  if (mirrorBusy.value[item.id]) return;
  const name = item.cloudPrefix || "_peerdav_mirror";
  if (!await confirmAction(t("drive.mirrorDeleteConfirm", { name }), t("drive.mirrorDelete"), { danger: true, confirmText: t("common.delete") })) return;
  await run(async (scope) => {
    await cloud.mirrorDeleteMapping(String(item.id));
    if (scope !== generation || disposed) return;
    await refresh(scope);
    if (scope === generation && !disposed) emit("changed");
  });
}
async function syncMirror(id, force = false) {
  if (mirrorBusy.value[id] || busy.value) return;
  mirrorBusy.value = { ...mirrorBusy.value, [id]: true };
  try {
    const result = await cloud.mirrorSync(id, force);
    mirrorResults.value = { ...mirrorResults.value, [id]: result };
    if (!disposed) emit("changed");
  } catch (error) {
    if (disposed) return;
    // 删除量超过安全阈值时先让用户确认，再以 force 重跑一次。
    if (error?.code === "MIRROR_MASS_DELETE" && !force) {
      mirrorBusy.value = { ...mirrorBusy.value, [id]: false };
      const confirmed = await confirmAction(
        t("drive.mirrorMassDeleteConfirm"),
        t("drive.mirrorMassDeleteTitle"),
        { danger: true, confirmText: t("common.delete") },
      );
      if (confirmed && !disposed) await syncMirror(id, true);
      return;
    }
    fail(error);
  } finally {
    if (!disposed) mirrorBusy.value = { ...mirrorBusy.value, [id]: false };
  }
}
async function cancelMirror(id) {
  try { await cloud.mirrorCancel(id); } catch (error) { fail(error); }
}
// 镜像映射的自动同步开关与间隔：直接回写后端，调度器随后会按新配置重建定时任务。
async function toggleMirrorSchedule(item, enabled) {
  await run(async (scope) => {
    await cloud.mirrorSaveMapping({ id: item.id, connectionId: item.connectionId, localRoot: item.localRoot, cloudPrefix: item.cloudPrefix, schedule: { enabled, interval: item.schedule?.interval || 900 } });
    if (scope !== generation || disposed) return;
    await refresh(scope);
  });
}
async function changeMirrorInterval(item, interval) {
  const value = Math.max(60, Number(interval) || 900);
  await run(async (scope) => {
    await cloud.mirrorSaveMapping({ id: item.id, connectionId: item.connectionId, localRoot: item.localRoot, cloudPrefix: item.cloudPrefix, schedule: { enabled: !!item.schedule?.enabled, interval: value } });
    if (scope !== generation || disposed) return;
    await refresh(scope);
  });
}
async function openPicker() {
  pickerOpen.value = true; pickerPrefix.value = ""; pickerNewName.value = "";
  await loadPicker("");
}
async function loadPicker(prefix) {
  pickerLoading.value = true;
  try {
    const fullPrefix = `_peerdav_mirror/${prefix}`;
    const page = await cloudObjects.list({ connectionId: props.connectionId, prefix: fullPrefix, next: null });
    pickerEntries.value = objectChildren(page.objects || [], fullPrefix).filter((entry) => entry.directory);
  } catch (error) { fail(error); pickerEntries.value = []; }
  finally { pickerLoading.value = false; }
}
async function openPickerFolder(entry) {
  const next = entry.key.replace(/^_peerdav_mirror\//, "");
  pickerPrefix.value = next; pickerNewName.value = "";
  await loadPicker(next);
}
function confirmPicker() {
  const trimmed = pickerNewName.value.trim().replace(/^\/+|\/+$/g, "");
  const combined = trimmed ? (pickerPrefix.value ? `${pickerPrefix.value}${trimmed}` : trimmed) : pickerPrefix.value;
  mirrorForm.value.cloudPrefix = combined.replace(/^\/+|\/+$/g, "");
  pickerOpen.value = false;
}
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
function mirrorStatusText(item) {
  const status = mirrorStatus.value[item.id];
  if (mirrorBusy.value[item.id]) return t("cloud.phaseScanning");
  if (status) return `${phaseLabel(status.phase)}${status.detail ? ` · ${status.detail}` : ""}`;
  return t("cloud.phaseIdle");
}
onMounted(async () => {
  try {
    const cleanup = await listen("app://cloud-progress", (event) => {
      const id = event.payload?.mappingId;
      if (disposed || !id) return;
      // append 映射：状态进入下方进度行；镜像映射：状态进入镜像条目。
      if (mappings.value.some((m) => m.id === id)) {
        data.value.statuses = [...(data.value.statuses || []).filter((s) => s.mappingId !== id), event.payload];
      }
      if ((data.value.mirrorMappings || []).some((m) => m.id === id)) {
        mirrorStatus.value = { ...mirrorStatus.value, [id]: event.payload };
        if (event.payload?.phase === "completed" || event.payload?.phase === "cancelled" || event.payload?.phase === "failed") {
          mirrorBusy.value = { ...mirrorBusy.value, [id]: false };
        }
      }
    });
    if (disposed) await cleanup(); else unlisten = cleanup;
  } catch (error) { if (!disposed) fail(error); }
});
onBeforeUnmount(() => { disposed = true; generation++; if (unlisten) Promise.resolve().then(unlisten).catch(fail); });
</script>

<template>
  <section class="sync-drives" :aria-label="t('drive.manage')">
    <h3>{{ t('drive.manage') }}</h3>

    <!-- 主流程：tab 化的「新建 / 链接已有」对话框。 -->
    <el-button class="open-create-drive" type="primary" :disabled="busy" @click="createDialogOpen = true">{{ t('drive.createTitle') }}</el-button>
    <p class="append-state" role="status">{{ t(`drive.${state}`) }}</p>
    <el-dialog :model-value="createDialogOpen" @update:model-value="closeCreateDialog" width="600" align-center
      :show-close="!busy" :close-on-click-modal="false" :close-on-press-escape="!busy" data-test="create-drive-dialog">
      <template #header="{ titleId, titleClass }">
        <div class="create-header">
          <span :id="titleId" :class="titleClass">{{ t('drive.createOrConnect') }}</span>
          <el-popover placement="bottom-start" trigger="click" :width="320" :show-after="0" popper-class="queue-help-popper">
            <div class="queue-help"><p>{{ t('cloud.safetyHint') }}</p></div>
            <template #reference>
              <button type="button" class="create-header__help" :aria-label="t('drive.helpLabel')">?</button>
            </template>
          </el-popover>
        </div>
      </template>
      <div class="drive-tabs" role="tablist" :aria-label="t('drive.createOrConnect')">
        <button type="button" role="tab" class="drive-tab" data-test="tab-create" :aria-selected="createTab === 'create'" :class="{ 'is-active': createTab === 'create' }" @click="createTab = 'create'">{{ t('drive.tabCreate') }}</button>
        <button type="button" role="tab" class="drive-tab" data-test="tab-link" :aria-selected="createTab === 'link'" :class="{ 'is-active': createTab === 'link' }" @click="createTab = 'link'">{{ t('drive.tabLink') }}</button>
      </div>
      <!-- 新建：加密开关切换「加密同步盘」与「明文镜像同步」。 -->
      <div v-if="createTab === 'create'">
        <label class="settings-row"><el-switch v-model="encrypted" class="create-encrypted-toggle" :disabled="busy" /><span>{{ t('drive.encryptedToggle') }}</span></label>
        <p class="hint">{{ t(encrypted ? 'drive.modeEncryptedHint' : 'drive.modeMirrorHint') }}</p>
        <form v-if="!encrypted" class="drive-create" @submit.prevent="createMirror">
          <label class="settings-row"><span>{{ t('drive.mirrorCloudFolder') }}</span>
            <el-input v-model="mirrorForm.cloudPrefix" class="mirror-cloud-prefix" :placeholder="t('drive.mirrorCloudFolderHint')" :disabled="busy" />
            <el-button class="pick-cloud-folder" :disabled="busy" @click="openPicker">{{ t('drive.pickCloudFolder') }}</el-button>
          </label>
          <label class="settings-row"><span>{{ t('drive.mirrorLocalFolder') }}</span>
            <el-input v-model="mirrorForm.localRoot" class="mirror-local-root" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" />
            <el-button class="choose-mirror-root" :disabled="busy" @click="chooseMirrorRoot">{{ t('cloud.choose') }}</el-button>
          </label>
        </form>
        <form v-else class="drive-create" @submit.prevent="create">
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
      </div>
      <!-- 链接已有同步盘：密码 + 已有同步盘下拉（旁有刷新）+ 本地目录。 -->
      <div v-else>
        <p class="hint">{{ t('drive.linkPasswordHint') }}</p>
        <div class="settings-rows append-settings" :aria-busy="busy">
          <label class="settings-row"><span>{{ t('cloud.password') }}</span><el-input v-model="password" class="append-join-password" type="password" autocomplete="current-password" :disabled="busy" /></label>
          <el-button class="append-discover" :disabled="busy || !connectionId" @click="discover">{{ t('drive.discover') }}</el-button>
          <label v-if="candidates.length" class="settings-row"><span>{{ t('drive.select') }}</span>
            <el-select v-model="selectedCandidateKey" class="append-candidate" :disabled="busy">
              <el-option v-for="(item, index) in candidates" :key="item.key" :value="item.key" :label="t('drive.number', { number: index + 1 })" />
            </el-select>
            <el-button class="append-refresh" :disabled="busy" @click="discover">{{ t('drive.refresh') }}</el-button>
          </label>
          <div v-if="candidate" class="append-trust">
            <p>{{ candidate.descriptor.mode === 'encrypted' ? t('cloud.encrypted') : t('cloudAppend.plain') }}</p>
            <details class="advanced-security"><summary>{{ t('drive.advancedSecurity') }}</summary>
              <p>{{ t('drive.fingerprint') }}: {{ candidate.fingerprint }}</p>
              <label class="settings-row"><span>{{ t('cloudAppend.trustedFingerprint') }}</span><el-input v-model="trustedFingerprint" class="append-fingerprint" :disabled="busy" /></label>
              <label class="settings-row"><el-switch v-model="acceptTofu" class="append-tofu" :disabled="busy" /><span>{{ t('drive.tofu') }}</span></label>
            </details>
            <label class="settings-row"><span>{{ t('drive.linkLocalFolder') }}</span><el-input v-model="linkForm.localRoot" class="link-local-root" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" /><el-button class="choose-link-root" :disabled="busy" @click="chooseLinkRoot">{{ t('cloud.choose') }}</el-button></label>
            <label v-if="joinedRepoId && linkRoots.length > 1" class="settings-row"><span>{{ t('drive.folder') }}</span><el-select v-model="linkForm.selectedRoot" class="link-root" :disabled="busy"><el-option v-for="root in linkRoots" :key="root" :value="root" :label="root" /></el-select></label>
            <p v-if="joinedRepoId && linkRoots.length > 1" class="hint">{{ t('drive.linkRootHint') }}</p>
            <label v-if="joinedRepoId && linkRoots.length === 0" class="settings-row"><span>{{ t('drive.folder') }}</span><el-input v-model="linkForm.newRoot" class="link-new-root" :placeholder="t('cloudAppend.newRoot')" :disabled="busy" /></label>
            <p v-if="joinedRepoId && linkRoots.length === 0" class="hint">{{ t('drive.linkNewRootHint') }}</p>
            <el-button v-if="!joinedRepoId || linkRoots.length === 1" class="append-join" :disabled="!canJoin" @click="join">{{ t('drive.connect') }}</el-button>
            <el-button v-else class="append-bind" :disabled="!linkForm.selectedRoot && !linkForm.newRoot.trim()" @click="confirmBind">{{ t('drive.linkBind') }}</el-button>
          </div>
        </div>
      </div>
      <template #footer>
        <el-button class="cancel-create-drive" :disabled="busy" @click="closeCreateDialog">{{ t('common.cancel') }}</el-button>
        <el-button v-if="createTab === 'create' && !encrypted" class="mirror-create" type="primary" :disabled="!canCreateMirror" :loading="busy" @click="createMirror">{{ t('drive.createMirror') }}</el-button>
        <el-button v-if="createTab === 'create' && encrypted" class="append-create" type="primary" :disabled="!canCreate" :loading="busy" @click="create">{{ t('drive.createAndBind') }}</el-button>
      </template>
    </el-dialog>

    <!-- 云端目录选择器：逐级浏览 _peerdav_mirror，可直接选择当前目录或在其下新建。 -->
    <el-dialog :model-value="pickerOpen" @update:model-value="pickerOpen = false" :title="t('drive.pickerTitle')" width="520" align-center data-test="cloud-folder-picker">
      <div class="picker-breadcrumbs">
        <button v-for="(crumb, index) in pickerBreadcrumbs" :key="index" type="button" class="picker-crumb" :disabled="pickerLoading" @click="pickerPrefix = crumb.prefix; loadPicker(crumb.prefix)">{{ crumb.name }}</button>
      </div>
      <p class="hint">{{ t('drive.pickerCurrent') }}: _peerdav_mirror/{{ pickerPrefix || '' }}</p>
      <ul v-if="pickerEntries.length" class="picker-list">
        <li v-for="entry in pickerEntries" :key="entry.key"><button type="button" class="picker-folder" data-test="picker-folder" :disabled="pickerLoading" @click="openPickerFolder(entry)">{{ entry.name }}</button></li>
      </ul>
      <p v-else-if="!pickerLoading" class="hint">{{ t('drive.pickerEmpty') }}</p>
      <label class="settings-row"><span>{{ t('drive.pickerNewName') }}</span><el-input v-model="pickerNewName" class="picker-new-name" :disabled="pickerLoading" /></label>
      <template #footer>
        <el-button class="picker-select" type="primary" @click="confirmPicker">{{ t('drive.pickerSelect') }}</el-button>
      </template>
    </el-dialog>

    <ul class="drive-list"><li v-for="repo in repositories" :key="repo.id" :class="{ 'is-active': repo.id === focusId }">{{ repo.label }} · {{ repo.binding?.appendMode === 'authenticated_plain' || repo.mode === 'plain' ? t('cloudAppend.plain') : t('cloud.encrypted') }}</li></ul>

    <h4>{{ t('drive.mirrorList') }}</h4>
    <p v-if="!mirrorMappings.length" class="hint">{{ t('cloudBrowser.noSyncDrives') }}</p>
    <div v-for="item in mirrorMappings" :key="item.id" class="cloud-item mirror-item" :class="{ 'is-active': item.id === focusId }">
      <span>{{ item.localRoot }} → _peerdav_mirror/{{ item.cloudPrefix || '' }}</span>
      <span class="mirror-status" role="status">{{ mirrorStatusText(item) }}</span>
      <span class="mirror-actions">
        <el-button v-if="!mirrorBusy[item.id]" :disabled="busy" @click="syncMirror(item.id)">{{ t('drive.mirrorSync') }}</el-button>
        <el-button v-else @click="cancelMirror(item.id)">{{ t('drive.mirrorCancel') }}</el-button>
        <el-button :disabled="mirrorBusy[item.id] || busy" @click="deleteMirror(item)">{{ t('drive.mirrorDelete') }}</el-button>
      </span>
      <label class="mirror-schedule">
        <el-switch :model-value="!!item.schedule?.enabled" :disabled="busy || mirrorBusy[item.id]" @update:model-value="toggleMirrorSchedule(item, $event)" />
        <span>{{ t('cloud.schedule') }}</span>
        <input :value="item.schedule?.interval || 900" class="mirror-interval" type="number" min="60" step="60" :disabled="busy || mirrorBusy[item.id] || !item.schedule?.enabled" @change="changeMirrorInterval(item, $event.target.value)" />
      </label>
      <details v-if="mirrorResults[item.id]?.conflicts?.length" class="mirror-conflicts">
        <summary>{{ t('drive.mirrorConflicts', { count: mirrorResults[item.id].conflicts.length }) }}</summary>
        <ul><li v-for="path in mirrorResults[item.id].conflicts" :key="path">{{ path }}</li></ul>
      </details>
      <details v-if="mirrorResults[item.id]?.failures?.length" class="mirror-failures">
        <summary>{{ t('drive.mirrorFailures', { count: mirrorResults[item.id].failures.length }) }}</summary>
        <ul><li v-for="failure in mirrorResults[item.id].failures" :key="failure.path">{{ failure.path }} · {{ failure.reason }}</li></ul>
      </details>
    </div>

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
.create-header { display: flex; align-items: center; gap: .5rem; }
.create-header__help { display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; border: 1px solid var(--app-border); border-radius: 50%; background: transparent; color: var(--app-text-muted); font-size: 11px; line-height: 1; cursor: pointer; }
.create-header__help:hover, .create-header__help:focus-visible { color: var(--app-text); border-color: var(--app-text-muted); outline: none; }
.drive-tabs { display: flex; gap: .25rem; margin-bottom: .8rem; border-bottom: 1px solid var(--app-border); }
.drive-tab { padding: .5rem .9rem; border: none; border-bottom: 2px solid transparent; background: transparent; color: var(--app-text-muted); font-size: 14px; cursor: pointer; }
.drive-tab.is-active { color: var(--app-text); border-bottom-color: var(--el-color-primary); }
.picker-breadcrumbs { display: flex; flex-wrap: wrap; gap: .25rem; margin-bottom: .4rem; }
.picker-crumb { padding: 2px 8px; border: 1px solid var(--app-border); border-radius: 6px; background: transparent; color: var(--app-text-secondary); font-size: 12.5px; cursor: pointer; }
.picker-crumb:hover { color: var(--app-text); }
.picker-list { list-style: none; padding: 0; margin: .4rem 0; max-height: 220px; overflow: auto; }
.picker-folder { width: 100%; text-align: left; padding: 6px 10px; border: 1px solid var(--app-border); border-radius: 6px; background: var(--app-surface-2); cursor: pointer; }
.picker-folder:hover { background: var(--app-surface-3); }
.mirror-item { flex-wrap: wrap; }
.mirror-status { color: var(--app-text-muted); font-size: 12.5px; }
.mirror-actions { display: inline-flex; gap: .4rem; }
.mirror-schedule { display: inline-flex; align-items: center; gap: .4rem; font-size: 12.5px; color: var(--app-text-muted); }
.mirror-interval { width: 76px; padding: 2px 6px; border: 1px solid var(--app-border); border-radius: 6px; background: var(--app-surface-2); color: var(--app-text); }
</style>
