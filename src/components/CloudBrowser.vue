<script setup>
import { computed, h, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { cloud, cloudObjects, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import FileGlyph from "./FileGlyph.vue";
import { errorStrategy, normalizeAppError } from "../lib/invoke";
import CloudSyncDrives from "./CloudSyncDrives.vue";
import { t } from "../lib/i18n";
const app = useAppContext();
const view = ref("files");
const activeDriveId = ref("");
const catalogDrives = ref([]);
let catalogVersion = 0;
const drives = computed(() => catalogDrives.value.map((item, index) => ({ ...item, label: t("drive.number", { number: index + 1 }) })));
function openManagement(id = "") { activeDriveId.value = id; view.value = "management"; }
async function refreshCatalog() {
  const connectionId = props.connectionId;
  const version = ++catalogVersion;
  try {
    const catalog = await cloud.list();
    if (disposed || version !== catalogVersion || connectionId !== props.connectionId) return;
    connections.value = catalog.connections;
    catalogDrives.value = [
      ...(catalog.repositories || []).filter((item) => item.connectionId === connectionId),
      ...(catalog.appendRepositories || []).filter((item) => item.connectionId === connectionId).map((item) => ({ id: item.binding.repoId })),
    ];
    if (activeDriveId.value && !catalogDrives.value.some((item) => item.id === activeDriveId.value)) {
      activeDriveId.value = ""; view.value = "files";
    }
  } catch (cause) {
    if (!disposed && version === catalogVersion && connectionId === props.connectionId) app.handleFailure(cause);
  }
}

const props = defineProps({ connectionId: { type: String, required: true } });
const connections = ref([]);
const prefix = ref("");
const root = ref("");
const objects = ref([]);
const next = ref(null);
const complete = ref(false);
const loading = ref(false);
const busy = ref(false);
const error = ref(null);
const protectedRoot = ref(false);
const prepared = ref(false);
const preparing = ref(false);
const preparationError = ref(null);
let preparationVersion = 0;
const connectionName = computed(() => connections.value.find((item) => String(item.id) === props.connectionId)?.name || t("cloudBrowser.title"));
const visibleError = computed(() => preparationError.value || error.value);
const failureStrategy = computed(() => visibleError.value ? errorStrategy(visibleError.value) : null);
const transfers = ref([]);
const partial = ref(null);
let disposed = false;
let requestId = 0;
let statusVersion = 0;
let polling = false;
let timer;
const children = computed(() => objectChildren(objects.value, prefix.value));
const hasProtectedObjects = computed(() => objects.value.some((item) => item.protected === true));
const targetProtected = (target) => protectedRoot.value || objects.value.some((item) => item.protected === true && item.key.startsWith(target));
const canMutate = computed(() => prepared.value && !preparing.value && !protectedRoot.value && !busy.value && !loading.value);
const current = () => ({ id: requestId, connectionId: props.connectionId, prefix: prefix.value });
const valid = (scope) => !disposed && scope.id === requestId && scope.connectionId === props.connectionId && scope.prefix === prefix.value;
function fail(cause, scope) {
  if (!valid(scope)) return;
  error.value = normalizeAppError(cause);
  if (cause?.code === "CLOUD_OBJECT_PROTECTED") protectedRoot.value = true;
  if (["CLOUD_PHYSICAL_IDENTITY_UNCONFIRMED", "CLOUD_PHYSICAL_BINDING_INVALID", "CLOUD_DISCOVERY_INCOMPLETE"].includes(cause?.code)) { prepared.value = false; preparationError.value = error.value; }
}
async function load(more = false) {
  if (more && (loading.value || !next.value)) return;
  const scope = current();
  const cursor = more ? next.value : null;
  loading.value = true; error.value = null;
  try {
    const page = await cloudObjects.list({ connectionId: scope.connectionId, prefix: scope.prefix, next: cursor });
    if (!valid(scope)) return;
    objects.value = [...new Map([...(more ? objects.value : []), ...page.objects].map((item) => [item.key, item])).values()];
    protectedRoot.value = protectedRoot.value || page.prefixProtected === true;
    next.value = page.next; complete.value = page.complete === true && page.next == null;
  } catch (cause) { fail(cause, scope); }
  finally { if (valid(scope)) loading.value = false; }
}
function navigate(value) {
  if (!value.startsWith(root.value)) return;
  requestId++; prefix.value = value; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false; partial.value = null;
  void load();
}
function up() { const path = prefix.value.slice(root.value.length); const relative = path.endsWith("/") ? path.slice(0, -1) : path; const slash = relative.lastIndexOf("/"); navigate(root.value + (slash < 0 ? "" : relative.slice(0, slash + 1))); }
async function operate(action) {
  if (busy.value) return;
  const scope = current(); busy.value = true; error.value = null;
  try { await action(scope); } catch (cause) { fail(cause, scope); }
  finally { if (valid(scope)) busy.value = false; }
}
async function prepare() {
  if (preparing.value) return;
  const connectionId = props.connectionId;
  const version = ++preparationVersion;
  const isCurrent = () => !disposed && version === preparationVersion && connectionId === props.connectionId;
  preparing.value = true; prepared.value = false; preparationError.value = null;
  try {
    const result = await cloudObjects.prepare({ connectionId });
    if (!isCurrent()) return;
    prepared.value = result?.discovered === true;
    if (!prepared.value) preparationError.value = { code: "CLOUD_DISCOVERY_INCOMPLETE", message: t("cloudBrowser.checkIncomplete") };
  } catch (cause) {
    if (isCurrent()) { preparationError.value = normalizeAppError(cause); if (cause?.code === "CLOUD_OBJECT_PROTECTED") protectedRoot.value = true; }
  } finally { if (isCurrent()) preparing.value = false; }
}
function remember(transfer, scope) {
  if (!valid(scope)) return;
  statusVersion++;
  transfers.value = [...transfers.value.filter((item) => item.transferId !== transfer.transferId), transfer];
  if (transfer.error) fail(transfer.error, scope);
}
async function upload() {
  if (!canMutate.value) return;
  await operate(async (scope) => {
    const selected = await cloudObjects.selectLocal({ connectionId: scope.connectionId, direction: "upload" });
    if (!valid(scope) || !selected?.token) return;
    if (typeof selected.fileName !== "string" || !selected.fileName) return;
    const key = scope.prefix + selected.fileName;
    const existing = objects.value.find((item) => item.key === key);
    if (existing?.protected === true || protectedRoot.value) return;
    if (existing && (!existing.etag || !await confirmAction(t("cloudBrowser.uploadOverwrite", { key, etag: existing.etag }), t("cloudBrowser.upload")))) return;
    if (!valid(scope)) return;
    remember(await cloudObjects.upload({ connectionId: scope.connectionId, localToken: selected.token, key, overwriteEtag: existing?.etag ?? null, overwriteConfirmed: Boolean(existing) }), scope);
  });
}
const downloadTargets = ref({});
async function retryDownload(item) {
  const scope = current();
  if (!await confirmAction(t("cloudBrowser.downloadRetry"), t("cloudBrowser.download")) || !valid(scope)) return;
  await download(item, true);
}
async function download(item, confirmed = false) {
  await operate(async (scope) => {
    const args = { connectionId: scope.connectionId, direction: "download" };
    let selected = await cloudObjects.selectLocal(args);
    if (!valid(scope) || !selected?.token) return;
    const run = (overwriteConfirmed) => cloudObjects.download({ connectionId: scope.connectionId, localToken: selected.token, key: item.key, fileName: item.name, overwriteConfirmed });
    const accept = (transfer) => { if (valid(scope)) downloadTargets.value[transfer.transferId] = { ...item }; remember(transfer, scope); };
    try { accept(await run(confirmed)); }
    catch (cause) {
      if (confirmed || cause.code !== "CLOUD_PRECONDITION" || !valid(scope)) throw cause;
      fail(cause, scope);
      if (!await confirmAction(t("cloudBrowser.downloadRetry"), t("cloudBrowser.download")) || !valid(scope)) return;
      selected = await cloudObjects.selectLocal(args);
      if (!valid(scope) || !selected?.token) return;
      error.value = null; accept(await run(true));
    }
  });
}
function validDeletePlan(plan, target) {
  return plan != null && Number.isSafeInteger(plan.objectCount) && plan.objectCount >= 0 && Array.isArray(plan.entries)
    && plan.entries.length === plan.objectCount && new Set(plan.entries.map((item) => item?.key)).size === plan.objectCount
    && plan.entries.every((item) => typeof item?.key === "string" && item.key.startsWith(target)
      && !/[\\\0:]/.test(item.key) && item.key.split("/").every((part) => part && part !== "." && part !== "..")
      && Number.isSafeInteger(item.size) && item.size >= 0 && (item.etag === null || typeof item.etag === "string"))
    && typeof plan.planId === "string" && plan.planId.length > 0 && Number.isFinite(plan.expiresAtUnix);
}
async function remove(target) {
  if (!canMutate.value || targetProtected(target)) return;
  await operate(async (scope) => {
    partial.value = null;
    const plan = await cloudObjects.deletePlan({ connectionId: scope.connectionId, prefix: target });
    if (!valid(scope)) return;
    if (!validDeletePlan(plan, target)) { error.value = { code: "CLOUD_CONFIRMATION_REQUIRED", message: t("cloudBrowser.invalidPlan") }; return; }
    const accepted = await confirmAction(h("div", [
      h("p", { style: { whiteSpace: "pre-wrap" } }, t("cloudBrowser.deleteConfirm", { prefix: JSON.stringify(target), count: plan.objectCount })),
      h("ul", { "data-test": "delete-keys", style: { maxHeight: "40vh", overflow: "auto", paddingLeft: "20px", overflowWrap: "anywhere", whiteSpace: "pre-wrap" } }, plan.entries.map((item) => h("li", { key: item.key }, JSON.stringify(item.key)))),
    ]), t("cloudBrowser.delete"));
    if (!accepted || !valid(scope)) return;
    if (Date.now() >= plan.expiresAtUnix * 1000) { error.value = { code: "CLOUD_DELETE_PLAN_STALE", message: t("cloudBrowser.stalePlan") }; return; }
    const result = await cloudObjects.deleteExecute({ connectionId: scope.connectionId, planId: plan.planId, confirmed: true });
    if (!valid(scope)) return;
    partial.value = result;
    for (const item of result.failed) if (item.error?.code === "CLOUD_OBJECT_PROTECTED") fail(item.error, scope);
    await load();
  });
}
async function acknowledge(item) {
  if (item.phase !== "unknown" || busy.value) return;
  await operate(async (scope) => {
    if (!await confirmAction(t("cloudBrowser.acknowledgeConfirm", { key: item.key == null ? t("cloudBrowser.unknownTarget") : JSON.stringify(item.key) }), t("cloudBrowser.acknowledge")) || !valid(scope)) return;
    // Invalidate polls started before this explicit local-state transition.
    statusVersion++;
    remember(await cloudObjects.recoveryAcknowledge({ transferId: item.transferId, connectionId: item.connectionId, connectionRevision: item.connectionRevision, confirmed: true }), scope);
  });
}
async function cancel(item) { await operate(async (scope) => remember(await cloudObjects.cancel(item.transferId), scope)); }
async function poll() {
  if (disposed || polling) return;
  polling = true; const scope = current(); const version = statusVersion;
  try {
    const result = await cloudObjects.status();
    if (valid(scope) && version === statusVersion) {
      transfers.value = result.filter((item) => item.connectionId === scope.connectionId);
      for (const item of transfers.value) if (item.error?.code === "CLOUD_OBJECT_PROTECTED") fail(item.error, scope);
    }
  } catch (cause) { if (version === statusVersion) fail(cause, scope); }
  finally { polling = false; }
}
watch(() => props.connectionId, () => {
  requestId++; statusVersion++; preparationVersion++; downloadTargets.value = {}; prepared.value = false; preparing.value = false; preparationError.value = null; protectedRoot.value = false; connections.value = [];
  view.value = "files"; activeDriveId.value = ""; catalogDrives.value = [];
  objects.value = []; transfers.value = []; partial.value = null; prefix.value = ""; root.value = ""; next.value = null; complete.value = false; busy.value = false; loading.value = false; error.value = null;
  void prepare();
  void load();
  void refreshCatalog();
}, { immediate: true });
onMounted(() => { void poll(); timer = setInterval(poll, 1500); });
onBeforeUnmount(() => { disposed = true; requestId++; statusVersion++; clearInterval(timer); });
</script>

<template>
  <section class="cloud-browser" :aria-label="t('cloudBrowser.title')">
    <div class="browser-body">
      <aside class="browser-sidebar">
        <div class="sidebar-identity"><strong>{{ connectionName }}</strong><span>{{ t('cloudBrowser.connectionHint') }}</span></div>
        <p class="sidebar-label">{{ t('cloudBrowser.syncDrives') }}</p>
        <button type="button" class="share-link all-files" :class="{ 'is-active': view === 'files' }" @click="view = 'files'; activeDriveId = ''">
          <FileGlyph :entry="{ isDir: true, name: t('cloudBrowser.allFiles') }" /><span>{{ t('cloudBrowser.allFiles') }}</span>
        </button>
        <button v-for="(drive, index) in drives" :key="index" type="button" class="share-link drive-link" :class="{ 'is-active': view === 'management' && activeDriveId === drive.id }" @click="openManagement(drive.id)">
          <FileGlyph :entry="{ isDir: true, name: drive.label }" /><span>{{ drive.label }}</span>
        </button>
        <p v-if="!drives.length" class="sidebar-empty">{{ t('cloudBrowser.noSyncDrives') }}</p>
        <button type="button" class="share-link create-drive-entry" :class="{ 'is-active': view === 'management' && !activeDriveId }" @click="openManagement()"><span>{{ t('drive.createOrConnect') }}</span></button>
      </aside>
      <main class="browser-main">
        <div v-if="view === 'management'" class="management-view">
          <header class="cloud-toolbar"><el-button class="back-to-files" @click="view = 'files'; activeDriveId = ''">{{ t('cloudBrowser.allFiles') }}</el-button><h2>{{ t('drive.manage') }}</h2></header>
          <div class="management-content"><CloudSyncDrives :key="connectionId" :connection-id="connectionId" :focus-id="activeDriveId" @changed="refreshCatalog" /></div>
        </div>
        <div v-else class="files-view">
    <div class="cloud-toolbar">
      <p v-if="preparing" role="status">{{ t('cloudBrowser.checking') }}</p>
      <el-tag v-else>{{ protectedRoot ? t('cloudBrowser.protected') : prepared ? t('cloudBrowser.editable') : t('cloudBrowser.checkFailed') }}</el-tag>
      <el-button v-if="!preparing && preparationError" data-test="recheck" :disabled="busy" @click="prepare">{{ t('cloudBrowser.recheck') }}</el-button>
    </div>
    <nav class="cloud-toolbar" :aria-label="t('cloudBrowser.directory')"><el-button data-test="up" :disabled="prefix === root" @click="up">{{ t('cloudBrowser.up') }}</el-button><code>{{ prefix || '/' }}</code><el-button data-test="refresh" :disabled="busy || loading" @click="navigate(prefix)">{{ t('cloudBrowser.refresh') }}</el-button></nav>
    <div v-if="visibleError" role="alert" :data-error-strategy="failureStrategy">{{ visibleError.message }}</div>
    <p v-if="loading" role="status">{{ t('cloudBrowser.loading') }}</p>
    <p v-if="!complete" data-test="incomplete">{{ t('cloudBrowser.incomplete') }}</p>
    <p v-if="!loading && complete && !children.length">{{ t('cloudBrowser.empty') }}</p>
    <ul class="cloud-tree" :aria-label="t('cloudBrowser.directory')">
      <li v-for="item in children" :key="item.key" :class="{ 'cloud-folder': item.directory }">
        <el-button v-if="item.directory" class="folder-link" @click="navigate(item.key)">{{ item.name }}</el-button>
        <span v-else>{{ item.name }} · {{ t('cloudBrowser.bytes', { size: item.size }) }}</span>
        <span v-if="item.protected">{{ t('cloudBrowser.protected') }}</span>
        <div class="cloud-actions"><el-button v-if="!item.directory" :disabled="busy" @click="download(item)">{{ t('cloudBrowser.download') }}</el-button><el-button :disabled="!canMutate || targetProtected(item.key)" @click="remove(item.key)">{{ t('cloudBrowser.delete') }}</el-button></div>
      </li>
    </ul>
    <el-button v-if="next" data-test="more" :disabled="loading" @click="load(true)">{{ t('cloudBrowser.more') }}</el-button>
    <div class="cloud-toolbar"><el-button data-test="upload" :disabled="!canMutate" @click="upload">{{ t('cloudBrowser.upload') }}</el-button><el-button data-test="delete-prefix" :disabled="!canMutate || hasProtectedObjects" @click="remove(prefix)">{{ t('cloudBrowser.deletePrefix') }}</el-button></div>
    <div v-if="partial" role="status"><p>{{ t('cloudBrowser.deleteResult', { deleted: partial.deleted.length, failed: partial.failed.length }) }}</p><ul><li v-for="item in partial.failed" :key="item.key">{{ item.key }} · {{ item.error.message }}</li></ul></div>
    <h3 v-if="transfers.length">{{ t('cloudBrowser.transfers') }}</h3>
    <ul v-if="transfers.length" class="cloud-tree cloud-transfers"><li v-for="item in transfers" :key="item.transferId"><span>{{ item.key == null ? t('cloudBrowser.unknownTarget') : JSON.stringify(item.key) }}<span v-if="item.sizeHint != null"> · {{ t('cloudBrowser.bytes', { size: item.sizeHint }) }}</span> · {{ t(`cloudBrowser.${item.direction}`) }} · {{ t(`cloudBrowser.phase.${item.phase}`) }}<strong v-if="item.recoveryRequired || item.phase === 'unknown'"> · {{ t('cloudBrowser.recovery') }}</strong><span v-if="item.error"> · {{ item.error.message }}</span></span><el-button v-if="item.phase === 'failed' && item.direction === 'download' && item.error?.code === 'CLOUD_PRECONDITION' && downloadTargets[item.transferId]" :disabled="busy" @click="retryDownload(downloadTargets[item.transferId])">{{ t('cloudBrowser.retryDownload') }}</el-button><p v-if="item.phase === 'acknowledged'">{{ t('cloudBrowser.acknowledgedNotice') }}</p><el-button v-if="item.phase === 'unknown'" :disabled="busy" @click="acknowledge(item)">{{ t('cloudBrowser.acknowledge') }}</el-button><el-button v-if="item.phase === 'running'" :disabled="busy" @click="cancel(item)">{{ t('cloudBrowser.cancel') }}</el-button></li></ul>
        </div>
      </main>
    </div>
  </section>
</template>

<style scoped>
.cloud-browser { flex: 1; min-width: 0; min-height: 0; overflow: hidden; display: flex; color: var(--app-text); }
.browser-body { flex: 1; min-width: 0; min-height: 0; display: grid; grid-template-columns: 200px minmax(0, 1fr); overflow: hidden; background: var(--app-surface); }
.browser-sidebar { min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 4px; padding: 14px 10px; overflow: auto; background: var(--app-surface-2); border-right: 1px solid var(--app-border); }
.sidebar-identity { display: flex; flex-direction: column; gap: 4px; padding: 0 8px 16px; min-width: 0; }
.sidebar-identity strong, .sidebar-identity span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sidebar-identity span, .sidebar-label, .sidebar-empty { font-size: 12px; color: var(--app-text-muted); }
.sidebar-label, .sidebar-empty { padding: 6px 8px; }
.share-link { display: flex; align-items: center; gap: 8px; width: 100%; min-width: 0; padding: 8px; border: 0; border-radius: 6px; background: transparent; color: var(--app-text); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.cloud-browser .share-link span, .cloud-browser .sidebar-identity span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.share-link :deep(.file-glyph) { flex: 0 0 18px; width: 18px; height: 18px; }
.share-link:hover, .share-link.is-active { background: var(--app-surface-3); }
.share-link.is-active { box-shadow: inset 3px 0 var(--el-color-primary); }
.browser-main { min-width: 0; min-height: 0; display: flex; overflow: hidden; }
.files-view, .management-view { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 12px; padding: 16px; overflow: hidden; }
.files-view { overflow-y: auto; }
.files-view > :not(.cloud-tree) { flex-shrink: 0; }
.files-view > .cloud-tree { flex: 1 1 auto; min-height: 0; overflow: auto; }
.files-view > .cloud-transfers { flex: 0 1 30%; }
.management-content { flex: 1; min-height: 0; overflow: auto; }
@media (max-width: 719px) {
  .browser-body { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto minmax(0, 1fr); }
  .browser-sidebar { flex-direction: row; align-items: center; border-right: 0; border-bottom: 1px solid var(--app-border); padding: 8px; }
  .sidebar-identity { flex: 0 0 auto; max-width: 140px; padding: 0 8px; }
  .sidebar-label, .sidebar-empty { display: none; }
  .share-link { flex: 0 0 auto; width: auto; max-width: 200px; white-space: nowrap; }
}
.cloud-browser p, .cloud-browser h2, .cloud-browser h3 { margin: 0; }
.cloud-browser p { color: var(--app-text-muted); line-height: 1.6; }
.cloud-toolbar, .cloud-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; min-width: 0; }
.cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 240px; max-width: 100%; min-width: 0; }
.cloud-tree { margin: 0; padding: 0; list-style: none; }
.cloud-tree li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px; border-bottom: 1px solid var(--app-border); }
.cloud-folder { border-left: 2px solid var(--el-color-primary); }
.cloud-browser span, .cloud-browser code, .cloud-browser [role=alert], .cloud-browser li { min-width: 0; overflow-wrap: anywhere; white-space: normal; }
.cloud-browser :deep(.el-button + .el-button) { margin-left: 0; }
.cloud-browser [role=alert] { color: var(--el-color-danger); }
@media (max-width: 480px) { .files-view, .management-view { padding: 10px; } .cloud-tree li { flex-wrap: wrap; align-items: flex-start; } .cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 100%; } .cloud-actions { width: 100%; } }
</style>
