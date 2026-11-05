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
const connectDialogOpen = ref(false);
const addMenuOpen = ref(false);
// 新建同步盘的三种形态：加密 Append、认证明文 Append、明文镜像。
const createMode = ref("encrypted");
const isMirror = computed(() => createMode.value === "mirror");
const needsPassword = computed(() => createMode.value !== "mirror");
const createModeHint = computed(() => createMode.value === "mirror"
  ? "drive.modeMirrorHint"
  : createMode.value === "authenticated_plain" ? "drive.modeAuthenticatedPlainHint" : "drive.modeEncryptedHint");
const passwordHint = computed(() => createMode.value === "authenticated_plain"
  ? "drive.passwordHintAuthenticatedPlain" : "drive.passwordHintEncrypted");
function resetCreateMode() { createMode.value = "encrypted"; }
function closeAddMenu(event) { if (!event.target?.closest?.(".add-menu")) addMenuOpen.value = false; }
function openCreateDialog() { addMenuOpen.value = false; connectDialogOpen.value = false; mappingEditorOpen.value = false; resetNewDrive(); resetCreateMode(); resetMirror(); state.value = "idle"; createDialogOpen.value = true; }
function openConnectDialog() { addMenuOpen.value = false; createDialogOpen.value = false; mappingEditorOpen.value = false; resetLink(); state.value = "idle"; connectDialogOpen.value = true; void discover(); }
// 镜像盘块的独立新建入口：复用新建对话框，但直接锁定为镜像形态，免去小白再选类型。
function openMirrorCreate() { addMenuOpen.value = false; connectDialogOpen.value = false; mappingEditorOpen.value = false; resetNewDrive(); resetMirror(); createMode.value = "mirror"; state.value = "idle"; createDialogOpen.value = true; }
function closeCreateDialog() { if (busy.value) return; createDialogOpen.value = false; resetNewDrive(); resetCreateMode(); resetMirror(); }
function closeConnectDialog() { if (busy.value) return; connectDialogOpen.value = false; resetLink(); }
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
function mappingStatus(item) {
  const status = statuses.value.find((s) => s.mappingId === item.id);
  return status ? `${phaseLabel(status.phase)}${status.detail ? ` · ${status.detail}` : ""}` : t("cloud.phaseIdle");
}
function repositoryLabel(item) { return repositories.value.find((r) => r.id === item.repositoryId)?.label || item.repositoryId; }
function repositoryType(repo) { return repo.binding?.appendMode === "authenticated_plain" ? t("drive.modeAuthenticatedPlain") : repo.mode === "plain" ? t("cloudAppend.plain") : t("cloud.encrypted"); }
function candidateModeLabel(item) { return item.descriptor.mode === "encrypted" ? t("drive.modeEncrypted") : t("drive.modeAuthenticatedPlain"); }
function candidateModeHint(item) { return item.descriptor.mode === "encrypted" ? t("drive.connectCandidateEncryptedHint") : t("drive.connectCandidatePlainHint"); }
function mappingRemote(item) { return `${repositoryLabel(item)} · ${item.logicalPath || "/"}`; }
function mirrorRemote(item) { return `_peerdav_mirror/${item.cloudPrefix || ""}`; }
function mirrorStats(item) {
  const result = mirrorResults.value[item.id];
  if (!result) return t("drive.statsEmpty");
  return t("drive.mirrorStats", {
    uploaded: result.uploaded || 0,
    downloaded: result.downloaded || 0,
    conflicts: result.conflicts?.length || 0,
    failures: result.failures?.length || 0,
  });
}
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
  generation++; busy.value = false; createDialogOpen.value = false; connectDialogOpen.value = false; addMenuOpen.value = false; resetSecrets(); resetNewDrive(); resetCreateMode(); resetLink(); resetMirror();
  mapping.value = { ...blankMapping(), connectionId: props.connectionId }; editingMappingId.value = null; mappingEditorOpen.value = false;
  preview.value = null; recovery.value = []; state.value = "idle"; mirrorBusy.value = {}; mirrorStatus.value = {}; mirrorResults.value = {};
  void run((scope) => refresh(scope));
}, { immediate: true });
async function create() {
  if (!canCreate.value || isMirror.value) return;
  await run(async (scope) => {
    state.value = "loading";
    const logicalPath = newDrive.value.logicalPath.trim() || folderName(newDrive.value.localRoot);
    const redacted = await cloud.createSyncDrive({ connectionId: props.connectionId, mode: createMode.value, password: newDrive.value.password, passwordConfirmation: newDrive.value.confirmation });
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
    resetNewDrive(); resetMirror(); resetCreateMode(); createDialogOpen.value = false; await refresh(scope);
    if (scope !== generation || disposed) return;
    emit("changed");
    if (bindingError) throw bindingError;
    state.value = "saved";
  });
}
async function createMirror() {
  if (!canCreateMirror.value || !isMirror.value) return;
  await run(async (scope) => {
    state.value = "loading";
    await cloud.mirrorSaveMapping({ id: null, connectionId: props.connectionId, localRoot: mirrorForm.value.localRoot.trim(), cloudPrefix: mirrorForm.value.cloudPrefix.trim(), schedule: { enabled: false, interval: 900 } });
    if (scope !== generation || disposed) return;
    resetMirror(); resetNewDrive(); resetCreateMode(); createDialogOpen.value = false; await refresh(scope);
    if (scope !== generation || disposed) return;
    emit("changed");
    state.value = "saved";
  });
}
function submitCreate() { return isMirror.value ? createMirror() : create(); }
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
  resetSecrets(); resetLink(); connectDialogOpen.value = false; await refresh(scope);
  if (scope !== generation || disposed) return;
  state.value = "saved"; emit("changed");
}
async function confirmBind() {
  const logicalPath = linkForm.value.selectedRoot || linkForm.value.newRoot.trim();
  if (!logicalPath || !joinedRepoId.value || !linkForm.value.localRoot.trim()) return;
  await run((scope) => bindLogicalPath(scope, logicalPath));
}
function blankMappingForCurrentDrive() {
  const focused = repositories.value.find((repo) => repo.id === props.focusId);
  const repositoryId = focused?.id || (repositories.value.length === 1 ? repositories.value[0].id : "");
  return { ...blankMapping(), connectionId: props.connectionId, repositoryId };
}
function resetMappingEditor() { editingMappingId.value = null; mapping.value = blankMappingForCurrentDrive(); preview.value = null; recovery.value = []; recoveryMappingId.value = ""; }
function closeMappingDialog() { if (busy.value) return; mappingEditorOpen.value = false; resetMappingEditor(); }
function editMapping(item) { addMenuOpen.value = false; createDialogOpen.value = false; connectDialogOpen.value = false; resetMappingEditor(); editingMappingId.value = item.id; mapping.value = { ...blankMapping(), ...item, schedule: { ...blankMapping().schedule, ...item.schedule } }; mappingEditorOpen.value = true; }
function newMapping() { addMenuOpen.value = false; createDialogOpen.value = false; connectDialogOpen.value = false; resetMappingEditor(); mappingEditorOpen.value = true; }
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
    resetMappingEditor(); mappingEditorOpen.value = false;
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
async function syncMirror(id, force = false, planId = null) {
  if (mirrorBusy.value[id] || busy.value) return;
  mirrorBusy.value = { ...mirrorBusy.value, [id]: true };
  try {
    const result = await cloud.mirrorSync(id, force, planId);
    mirrorResults.value = { ...mirrorResults.value, [id]: result };
    if (!disposed) emit("changed");
  } catch (error) {
    if (disposed) return;
    // The backend binds approval to the displayed immutable snapshot. Never
    // automatically approve a changed plan or retry without its plan token.
    const merge = error?.code === "MIRROR_CONFIRM_MERGE";
    if ((merge || error?.code === "MIRROR_MASS_DELETE") && !planId && error.context?.planId) {
      const paths = Array.isArray(error.context.paths) ? error.context.paths.join("\n") : "";
      const confirmed = await confirmAction(
        `${t(merge ? "drive.mirrorMergeConfirm" : "drive.mirrorMassDeleteConfirm")}\n\n${paths}`,
        t(merge ? "drive.mirrorMergeTitle" : "drive.mirrorMassDeleteTitle"),
        { danger: !merge, confirmText: t(merge ? "confirm.ok" : "common.delete") },
      );
      if (confirmed && !disposed) {
        mirrorBusy.value = { ...mirrorBusy.value, [id]: false };
        await syncMirror(id, !merge, error.context.planId);
      }
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
onMounted(() => document.addEventListener("click", closeAddMenu));
onBeforeUnmount(() => document.removeEventListener("click", closeAddMenu));
onBeforeUnmount(() => { disposed = true; generation++; if (unlisten) Promise.resolve().then(unlisten).catch(fail); });
</script>

<template>
  <section class="sync-drives" :aria-label="t('drive.manage')">
    <p v-if="state !== 'idle'" class="append-state" role="status">{{ t(`drive.${state}`) }}</p>

    <div class="drive-roles">
      <!-- 同步盘：一行对应一个本地目录映射；盘本身通过上方列表和云端目录列体现。 -->
      <section class="drive-group" data-test="sync-group">
        <header class="role-heading">
          <span class="role-icon" aria-hidden="true">
            <svg viewBox="0 0 24 24"><path d="M4 12a8 8 0 0 1 13.7-5.6" /><path d="M20 12a8 8 0 0 1-13.7 5.6" /><path d="M17.6 3.4v3.4h-3.4" /><path d="M6.4 20.6v-3.4h3.4" /></svg>
          </span>
          <h3 class="group-title">{{ t('drive.tabSync') }}<span v-if="mappings.length" class="count-badge">{{ mappings.length }}</span></h3>
          <div class="add-menu" @keydown.esc="addMenuOpen = false">
            <button type="button" class="add-menu-trigger" :aria-label="t('drive.addMenu')" :aria-expanded="addMenuOpen" aria-haspopup="menu" @click.stop="addMenuOpen = !addMenuOpen">+</button>
            <div v-if="addMenuOpen" class="add-menu-list" role="menu">
              <button type="button" role="menuitem" class="open-create-drive add-create-drive" @click="openCreateDialog">{{ t('drive.createTitle') }}</button>
              <button type="button" role="menuitem" class="add-connect-drive" @click="openConnectDialog">{{ t('drive.connectRemote') }}</button>
              <button type="button" role="menuitem" class="new-mapping add-local-folder" @click="newMapping">{{ t('drive.addFolder') }}</button>
            </div>
          </div>
        </header>
        <div class="drive-table-wrap">
      <ul class="drive-list"><li v-for="repo in repositories" :key="repo.id" :class="{ 'is-active': repo.id === focusId }">{{ repo.label }} · {{ repositoryType(repo) }}</li></ul>
      <table class="drive-table">
        <thead><tr><th>{{ t('cloud.localRoot') }}</th><th>{{ t('drive.remoteFolder') }}</th><th>{{ t('drive.status') }}</th><th>{{ t('drive.manageActions') }}</th></tr></thead>
        <tbody>
          <tr v-for="item in mappings" :key="item.id" class="mapping-item" :class="{ 'is-active': item.repositoryId === focusId }">
            <td class="mapping-local-cell">{{ item.localRoot }}</td>
            <td class="mapping-remote-cell">{{ mappingRemote(item) }}</td>
            <td class="mapping-status" role="status">{{ mappingStatus(item) }}</td>
            <td class="mapping-actions">
              <el-button class="mapping-edit" :disabled="busy" @click="editMapping(item)">{{ t('cloud.edit') }}</el-button>
              <el-button class="mapping-preview" :disabled="busy" @click="showPreview(item)">{{ t('cloud.preview') }}</el-button>
              <el-button class="mapping-cancel" :disabled="busy" @click="cancel(item)">{{ t('cloud.cancel') }}</el-button>
              <el-button class="mapping-recovery" :disabled="busy" @click="loadRecovery(item)">{{ t('cloud.recovery') }}</el-button>
              <el-button class="mapping-remove" :disabled="busy" @click="deleteMapping(item)">{{ t('drive.removeMapping') }}</el-button>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-if="!mappings.length" class="hint table-note">{{ t('drive.noLocalFolders') }}</p>
      <div v-if="preview" class="cloud-preview">
        <p>{{ t('cloud.confirmPreview') }}</p>
        <p>{{ t('cloud.previewUploads') }}: {{ preview.summary?.uploads ?? 0 }} · {{ t('cloud.previewDownloads') }}: {{ preview.summary?.downloads ?? 0 }} · {{ t('cloud.previewDeletes') }}: {{ preview.summary?.deletes ?? 0 }} · {{ t('cloud.previewConflicts') }}: {{ preview.summary?.conflicts ?? 0 }}</p>
        <pre v-if="typeof preview.summary === 'string'">{{ preview.summary }}</pre>
        <el-button :disabled="busy" @click="applyPreview">{{ t('cloud.sync') }}</el-button>
      </div>
      <div v-if="recovery.length" class="recovery-list">
        <p class="hint">{{ t('drive.recoveryFor', { path: recoveryMappingId }) }}</p>
        <ul><li v-for="record in recovery" :key="record.id">{{ record.relativePath }} · {{ record.phase }}<el-button :disabled="busy" @click="restore(record)">{{ t('cloud.restore') }}</el-button></li></ul>
      </div>
    </div>
      </section>

      <!-- 镜像盘：保持目录结构的明文双向镜像，独立于加密同步盘。 -->
      <section class="drive-group" data-test="mirror-group">
        <header class="role-heading">
          <span class="role-icon role-icon--mirror" aria-hidden="true">
            <svg viewBox="0 0 24 24"><path d="M12 3v18" /><path d="M9 7 4 12l5 5" /><path d="M15 7l5 5-5 5" /></svg>
          </span>
          <h3 class="group-title">{{ t('drive.tabMirror') }}<span v-if="mirrorMappings.length" class="count-badge">{{ mirrorMappings.length }}</span></h3>
          <el-button class="list-pill new-mirror-drive" type="primary" round size="small" :disabled="busy" @click="openMirrorCreate">{{ t('drive.createMirror') }}</el-button>
        </header>
        <div class="drive-table-wrap">
      <table class="drive-table">
        <thead><tr><th>{{ t('cloud.localRoot') }}</th><th>{{ t('drive.mirrorCloudFolder') }}</th><th>{{ t('drive.status') }}</th><th>{{ t('drive.stats') }}</th><th>{{ t('drive.manageActions') }}</th></tr></thead>
        <tbody>
          <tr v-for="item in mirrorMappings" :key="item.id" class="mirror-item" :class="{ 'is-active': item.id === focusId }">
            <td>{{ item.localRoot }}</td>
            <td class="mono">{{ mirrorRemote(item) }}</td>
            <td class="mirror-status" role="status">{{ mirrorStatusText(item) }}</td>
            <td class="mirror-stats">{{ mirrorStats(item) }}</td>
            <td class="mirror-actions">
              <el-button v-if="!mirrorBusy[item.id]" :disabled="busy" @click="syncMirror(item.id)">{{ t('drive.mirrorSync') }}</el-button>
              <el-button v-else @click="cancelMirror(item.id)">{{ t('drive.mirrorCancel') }}</el-button>
              <el-button :disabled="mirrorBusy[item.id] || busy" @click="deleteMirror(item)">{{ t('drive.mirrorDelete') }}</el-button>
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
            </td>
          </tr>
        </tbody>
      </table>
      <p v-if="!mirrorMappings.length" class="hint table-note">{{ t('drive.noMirrorDrives') }}</p>
        </div>
      </section>
    </div>

    <!-- 新建同步盘：加密 / 认证明文 / 明文镜像三选一。 -->
    <el-dialog :model-value="createDialogOpen" @update:model-value="closeCreateDialog" width="600" align-center class="create-drive-dialog"
      :show-close="!busy" :close-on-click-modal="false" :close-on-press-escape="!busy" data-test="create-drive-dialog">
      <template #header="{ titleId, titleClass }">
        <div class="create-header">
          <span :id="titleId" :class="titleClass">{{ t('drive.createTitle') }}</span>
          <el-popover placement="bottom-start" trigger="click" :width="320" :show-after="0" popper-class="queue-help-popper">
            <div class="queue-help"><p>{{ t('cloud.safetyHint') }}</p></div>
            <template #reference>
              <button type="button" class="create-header__help" :aria-label="t('drive.helpLabel')">?</button>
            </template>
          </el-popover>
        </div>
      </template>
      <label class="settings-row"><span>{{ t('drive.createMode') }}</span>
        <el-select v-model="createMode" class="create-mode" :disabled="busy">
          <el-option value="encrypted" :label="t('drive.modeEncrypted')" />
          <el-option value="authenticated_plain" :label="t('drive.modeAuthenticatedPlain')" />
          <el-option value="mirror" :label="t('drive.modeMirror')" />
        </el-select>
      </label>
      <p class="hint create-mode-hint">{{ t(createModeHint) }}</p>
      <form class="drive-create" @submit.prevent="submitCreate">
        <template v-if="needsPassword">
          <label class="settings-row"><span>{{ t('cloud.password') }}</span>
            <el-input v-model="newDrive.password" class="append-password" type="password" autocomplete="new-password" :disabled="busy" />
          </label>
          <label class="settings-row"><span>{{ t('cloud.passwordConfirm') }}</span>
            <el-input v-model="newDrive.confirmation" class="append-confirmation" type="password" autocomplete="new-password" :disabled="busy" />
          </label>
          <p class="hint">{{ t(passwordHint) }}</p>
        </template>
        <template v-if="isMirror">
          <label class="settings-row"><span>{{ t('drive.mirrorCloudFolder') }}</span>
            <div class="field-inline">
              <el-input v-model="mirrorForm.cloudPrefix" class="mirror-cloud-prefix" :placeholder="t('drive.mirrorCloudFolderHint')" :disabled="busy" />
              <el-button class="pick-cloud-folder" :disabled="busy" @click="openPicker">{{ t('drive.pickCloudFolder') }}</el-button>
            </div>
          </label>
          <label class="settings-row"><span>{{ t('drive.mirrorLocalFolder') }}</span>
            <div class="field-inline">
              <el-input v-model="mirrorForm.localRoot" class="mirror-local-root" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" />
              <el-button class="choose-mirror-root" :disabled="busy" @click="chooseMirrorRoot">{{ t('cloud.choose') }}</el-button>
            </div>
          </label>
        </template>
        <template v-else>
          <label class="settings-row"><span>{{ t('drive.localFolder') }}</span>
            <div class="field-inline">
              <el-input v-model="newDrive.localRoot" class="new-drive-local" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" />
              <el-button class="choose-new-root" :disabled="busy" @click="chooseNewRoot">{{ t('cloud.choose') }}</el-button>
            </div>
          </label>
          <label class="settings-row"><span>{{ t('drive.remoteFolder') }}</span>
            <el-input v-model="newDrive.logicalPath" class="new-drive-folder" :placeholder="t('drive.folderDefaultHint')" :disabled="busy" />
          </label>
        </template>
      </form>
      <template #footer>
        <el-button class="cancel-create-drive" :disabled="busy" @click="closeCreateDialog">{{ t('common.cancel') }}</el-button>
        <el-button v-if="isMirror" class="mirror-create" type="primary" :disabled="!canCreateMirror" :loading="busy" @click="createMirror">{{ t('drive.createMirror') }}</el-button>
        <el-button v-else class="append-create" type="primary" :disabled="!canCreate" :loading="busy" @click="create">{{ t('drive.createAndBind') }}</el-button>
      </template>
    </el-dialog>

    <!-- 连接远端同步盘：发现 anchor、校验密码/指纹，再绑定本地目录。 -->
    <el-dialog :model-value="connectDialogOpen" @update:model-value="closeConnectDialog" width="600" align-center
      :show-close="!busy" :close-on-click-modal="false" :close-on-press-escape="!busy" data-test="connect-drive-dialog">
      <template #header="{ titleId, titleClass }">
        <div class="create-header">
          <span :id="titleId" :class="titleClass">{{ t('drive.connectRemote') }}</span>
          <el-popover placement="bottom-start" trigger="click" :width="320" :show-after="0" popper-class="queue-help-popper">
            <div class="queue-help"><p>{{ t('cloud.safetyHint') }}</p></div>
            <template #reference>
              <button type="button" class="create-header__help" :aria-label="t('drive.helpLabel')">?</button>
            </template>
          </el-popover>
        </div>
      </template>
      <p class="hint">{{ t('drive.connectRemoteHint') }}</p>
      <div class="settings-rows append-settings" :aria-busy="busy">
        <label class="settings-row"><span>{{ t('drive.discoveredDrives') }}</span>
          <div class="field-inline">
            <el-select v-if="candidates.length" v-model="selectedCandidateKey" class="append-candidate" :disabled="busy">
              <el-option v-for="(item, index) in candidates" :key="item.key" :value="item.key" :label="`${t('drive.number', { number: index + 1 })} · ${candidateModeLabel(item)}`" />
            </el-select>
            <el-button class="append-discover" :disabled="busy || !connectionId" @click="discover">{{ candidates.length ? t('drive.refresh') : t('drive.discover') }}</el-button>
          </div>
        </label>
        <p v-if="state === 'empty' && !busy" class="hint">{{ t('drive.empty') }}</p>
        <div v-if="candidate" class="append-trust">
          <p class="connect-candidate-type">{{ candidateModeLabel(candidate) }}</p>
          <p class="hint">{{ candidateModeHint(candidate) }}</p>
          <label class="settings-row"><span>{{ t('cloud.password') }}</span><el-input v-model="password" class="append-join-password" type="password" autocomplete="current-password" :disabled="busy" /></label>
          <p class="hint">{{ t(candidate.descriptor.mode === 'encrypted' ? 'drive.passwordHintEncrypted' : 'drive.passwordHintAuthenticatedPlain') }}</p>
          <details class="advanced-security"><summary>{{ t('drive.advancedSecurity') }}</summary>
            <p>{{ t('drive.fingerprint') }}: {{ candidate.fingerprint }}</p>
            <label class="settings-row"><span>{{ t('cloudAppend.trustedFingerprint') }}</span><el-input v-model="trustedFingerprint" class="append-fingerprint" :disabled="busy" /></label>
            <label class="settings-row switch-row"><el-switch v-model="acceptTofu" class="append-tofu" :disabled="busy" /><span>{{ t('drive.tofu') }}</span></label>
          </details>
          <label class="settings-row"><span>{{ t('drive.linkLocalFolder') }}</span><div class="field-inline"><el-input v-model="linkForm.localRoot" class="link-local-root" :placeholder="t('drive.chooseFolderHint')" :disabled="busy" /><el-button class="choose-link-root" :disabled="busy" @click="chooseLinkRoot">{{ t('cloud.choose') }}</el-button></div></label>
          <label v-if="joinedRepoId && linkRoots.length > 1" class="settings-row"><span>{{ t('drive.folder') }}</span><el-select v-model="linkForm.selectedRoot" class="link-root" :disabled="busy"><el-option v-for="root in linkRoots" :key="root" :value="root" :label="root" /></el-select></label>
          <p v-if="joinedRepoId && linkRoots.length > 1" class="hint">{{ t('drive.linkRootHint') }}</p>
          <label v-if="joinedRepoId && linkRoots.length === 0" class="settings-row"><span>{{ t('drive.folder') }}</span><el-input v-model="linkForm.newRoot" class="link-new-root" :placeholder="t('cloudAppend.newRoot')" :disabled="busy" /></label>
          <p v-if="joinedRepoId && linkRoots.length === 0" class="hint">{{ t('drive.linkNewRootHint') }}</p>
        </div>
      </div>
      <template #footer>
        <el-button class="cancel-connect-drive" :disabled="busy" @click="closeConnectDialog">{{ t('common.cancel') }}</el-button>
        <el-button v-if="!joinedRepoId || linkRoots.length === 1" class="append-join" :disabled="!canJoin" @click="join">{{ t('drive.connect') }}</el-button>
        <el-button v-else class="append-bind" :disabled="!linkForm.selectedRoot && !linkForm.newRoot.trim()" @click="confirmBind">{{ t('drive.linkBind') }}</el-button>
      </template>
    </el-dialog>

    <!-- 本地目录：给本机已有同步盘新增或编辑一个映射。 -->
    <el-dialog :model-value="mappingEditorOpen" @update:model-value="closeMappingDialog" :title="editingMappingId ? t('drive.editLocalFolder') : t('drive.addFolder')" width="560" align-center
      :show-close="!busy" :close-on-click-modal="false" :close-on-press-escape="!busy" data-test="mapping-dialog">
      <div class="settings-rows mapping-editor">
        <label class="settings-row"><span>{{ t('drive.targetDrive') }}</span>
          <el-select v-if="repositories.length > 1" v-model="mapping.repositoryId" class="mapping-repository" :disabled="busy || !!editingMappingId"><el-option v-for="repo in repositories" :key="repo.id" :value="repo.id" :label="repo.label + ' · ' + repositoryType(repo)" /></el-select>
          <span v-else class="mapping-drive-static">{{ repositories[0] ? `${repositories[0].label} · ${repositoryType(repositories[0])}` : t('drive.noDrivesForMapping') }}</span>
        </label>
        <p class="hint">{{ t('drive.mappingScopeHint') }}</p>
        <label class="settings-row"><span>{{ t('cloud.localRoot') }}</span><div class="field-inline"><el-input v-model="mapping.localRoot" class="mapping-local" :disabled="busy" /><el-button class="choose-mapping-root" :disabled="busy" @click="chooseRoot">{{ t('cloud.choose') }}</el-button></div></label>
        <label class="settings-row"><span>{{ t('drive.folder') }}</span><el-select v-if="selectedRepository?.append && roots.length" v-model="mapping.logicalPath" class="append-root" :disabled="busy"><el-option v-for="(root, index) in roots" :key="root" :value="root" :label="t('drive.folderNumber', { number: index + 1, name: root })" /></el-select><el-input v-else v-model="mapping.logicalPath" class="mapping-remote" :placeholder="t('drive.newFolder')" :disabled="busy" /></label>
        <p>{{ t('drive.folderHint') }}</p>
        <!-- 新映射的自动同步由后端在首次保存时保持关闭，仅在编辑现有映射时提供开关。 -->
        <label v-if="editingMappingId" class="settings-row"><span>{{ t('cloud.schedule') }}</span><div class="field-inline"><el-switch v-model="mapping.schedule.enabled" :disabled="busy" /><input v-model.number="mapping.schedule.interval" :disabled="busy" type="number" min="60" step="60" /></div></label>
      </div>
      <template #footer>
        <el-button class="cancel-mapping" :disabled="busy" @click="closeMappingDialog">{{ t('common.cancel') }}</el-button>
        <el-button class="mapping-save" type="primary" :disabled="busy" @click="saveMapping">{{ editingMappingId ? t('cloud.update') : t('cloud.save') }}</el-button>
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
  </section>
</template>

<style scoped>
.drive-roles { flex: 1 1 auto; min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 24px; }
.drive-group { min-width: 0; }
.role-heading { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 11px; margin-bottom: 10px; }
.role-heading .group-title { margin: 0; }
.role-icon { width: 32px; height: 32px; flex: none; display: inline-flex; align-items: center; justify-content: center; border-radius: 10px; color: var(--el-color-primary); background: color-mix(in oklab, var(--el-color-primary) 10%, var(--app-surface-2)); }
.role-icon--mirror { color: color-mix(in oklab, var(--accent-green) 72%, var(--app-text)); background: color-mix(in oklab, var(--accent-green) 10%, var(--app-surface-2)); }
.role-icon svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
.add-menu { position: relative; }
.add-menu-trigger { display: inline-flex; align-items: center; justify-content: center; width: 24px; height: 24px; padding: 0; border: 1px solid var(--app-border); border-radius: 6px; background: transparent; color: var(--app-text-secondary); font-size: 16px; line-height: 1; cursor: pointer; }
.add-menu-trigger:hover, .add-menu-trigger[aria-expanded="true"] { background: var(--app-surface-2); color: var(--app-text); }
.add-menu-list { position: absolute; right: 0; top: calc(100% + 4px); z-index: 30; display: grid; min-width: 190px; padding: 4px; border: 1px solid var(--app-border); border-radius: 10px; background: var(--app-surface); box-shadow: 0 12px 28px rgba(22, 32, 46, 0.16); }
.add-menu-list button { padding: 8px 10px; border: 0; border-radius: 6px; background: transparent; color: var(--app-text); font: inherit; font-size: 13px; text-align: left; cursor: pointer; }
.add-menu-list button:hover, .add-menu-list button:focus-visible { background: var(--app-surface-2); outline: none; }
.sync-drives { display: flex; flex-direction: column; min-width: 0; height: 100%; min-height: 0; overflow-wrap: anywhere; }
.drive-list { padding: 0; margin: 0 0 .6rem; list-style: none; }
.drive-list li { padding: 6px 8px; border-radius: 6px; }
.drive-list .is-active { background: var(--app-surface-2); box-shadow: inset 3px 0 var(--el-color-primary); }
.drive-create { margin: 0; }
.drive-create h4 { margin: 0 0 .4rem; }
.settings-row { display: flex; flex-direction: column; gap: 6px; align-items: stretch; margin: .6rem 0; font-size: 13px; font-weight: 600; color: var(--app-text-strong); }
.switch-row { flex-direction: row; align-items: center; gap: 8px; }
.field-inline { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.field-inline :deep(.el-input), .field-inline :deep(.el-select) { flex: 1 1 0; min-width: 0; width: auto; }
.field-inline :deep(.el-button), .field-inline :deep(.el-switch) { flex: 0 0 auto; }
.field-inline input[type="number"] { width: 90px; padding: 4px 8px; border: 1px solid var(--app-border); border-radius: 6px; font: inherit; color: var(--app-text); background: var(--app-surface); }
.sync-drives :deep(.el-input), .sync-drives :deep(.el-select) { min-width: 0; max-width: 100%; }
.drive-table-wrap { min-width: 0; }
.drive-table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
.drive-table th, .drive-table td { padding: 8px; border-bottom: 1px solid var(--app-border); text-align: left; vertical-align: top; }
.drive-table th { position: sticky; top: 0; z-index: 1; background: var(--app-surface); color: var(--app-text-muted); font-weight: 500; white-space: nowrap; }
.table-note { margin: 12px 8px 0; color: var(--app-text-muted); font-size: 12px; font-style: italic; line-height: 1.5; }
.drive-table tr.is-active { background: var(--app-surface-2); box-shadow: inset 3px 0 var(--el-color-primary); }
.mapping-actions, .mirror-actions { display: flex; flex-wrap: wrap; gap: .35rem; align-items: center; }
.mapping-actions :deep(.el-button), .mirror-actions :deep(.el-button) { margin-left: 0; }
.mirror-actions { flex-direction: column; align-items: flex-start; }
.recovery-list ul { padding-left: 1.2rem; }
.cloud-preview pre { white-space: pre-wrap; }
summary { cursor: pointer; padding: .7rem 0; }
.create-header { display: flex; align-items: center; gap: .5rem; }
.create-header__help { display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; padding: 0; border: 1px solid var(--app-border); border-radius: 50%; background: transparent; color: var(--app-text-muted); font-size: 11px; line-height: 1; cursor: pointer; }
.create-header__help:hover, .create-header__help:focus-visible { color: var(--app-text); border-color: var(--app-text-muted); outline: none; }
.picker-breadcrumbs { display: flex; flex-wrap: wrap; gap: .25rem; margin-bottom: .4rem; }
.picker-crumb { padding: 2px 8px; border: 1px solid var(--app-border); border-radius: 6px; background: transparent; color: var(--app-text-secondary); font-size: 12.5px; cursor: pointer; }
.picker-crumb:hover { color: var(--app-text); }
.picker-list { list-style: none; padding: 0; margin: .4rem 0; max-height: 220px; overflow: auto; }
.picker-folder { width: 100%; text-align: left; padding: 6px 10px; border: 1px solid var(--app-border); border-radius: 6px; background: var(--app-surface-2); cursor: pointer; }
.picker-folder:hover { background: var(--app-surface-3); }
.mirror-status, .mapping-status { color: var(--app-text-muted); font-size: 12.5px; }
.connect-candidate-type { font-weight: 600; color: var(--app-text); }
.mapping-drive-static { color: var(--app-text-secondary); }
.mirror-stats { color: var(--app-text-muted); white-space: nowrap; }
.mirror-schedule { display: inline-flex; align-items: center; gap: .4rem; font-size: 12.5px; color: var(--app-text-muted); }
.mirror-interval { width: 76px; padding: 2px 6px; border: 1px solid var(--app-border); border-radius: 6px; background: var(--app-surface-2); color: var(--app-text); }
/* 对话框沿用网盘设置页风格：覆盖层在标题栏下方 flex 居中，弹窗贴合内容，
   展开后超过 max-height 才由主体滚动；底部按钮以 flex gap 分隔，避免粘连。 */
:deep(.el-overlay-dialog) { box-sizing: border-box; display: flex; align-items: center; justify-content: center; padding-top: var(--titlebar-height, 42px); padding-bottom: 12px; }
:deep(.el-dialog) { box-sizing: border-box; display: flex; flex-direction: column; max-height: calc(100vh - var(--titlebar-height, 42px) - 20px); margin: 0 auto; padding: 0; }
:deep(.el-dialog__header) { box-sizing: border-box; margin: 0; padding: 18px 15px 10px; }
:deep(.el-dialog__body) { box-sizing: border-box; flex: 1 1 auto; overflow-y: auto; padding: 6px 15px; }
:deep(.el-dialog__footer) { box-sizing: border-box; display: flex; justify-content: flex-end; gap: 12px; padding: 10px 15px 18px; }
:deep(.el-dialog__footer .el-button + .el-button) { margin-left: 0; }
</style>
