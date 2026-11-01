<script setup>
import { computed, h, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { cloud, cloudObjects, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import FileGlyph from "./FileGlyph.vue";
import { fileKind } from "../lib/fileKinds";
import { errorStrategy, normalizeAppError } from "../lib/invoke";
import CloudSyncDrives from "./CloudSyncDrives.vue";
import { t } from "../lib/i18n";
const app = useAppContext();
const view = ref("files");
const activeDriveId = ref("");
const catalogDrives = ref([]);
const buckets = ref([]);
const activeBucket = ref("");
// View modes mirror the LAN browser: icons, list, columns and gallery.
const VIEW_STORAGE_KEY = "peerdav.cloudView";
const VIEW_MODES = ["icons", "list", "columns", "gallery"];
const viewMode = ref(readViewMode());
function readViewMode() {
  try {
    const stored = globalThis.localStorage?.getItem(VIEW_STORAGE_KEY);
    return VIEW_MODES.includes(stored) ? stored : "list";
  } catch { return "list"; }
}
function setViewMode(mode) {
  if (!VIEW_MODES.includes(mode)) return;
  viewMode.value = mode;
  try { globalThis.localStorage?.setItem(VIEW_STORAGE_KEY, mode); } catch { /* a disabled storage must not block switching views */ }
  if (mode === "columns") columnStack.value = [];
}
// Column view drills into subdirectories without leaving the current folder:
// the first column is always the current directory, the stack holds deeper ones.
const columnStack = ref([]);
const columns = computed(() => [{ prefix: prefix.value, entries: children.value, loading: false }, ...columnStack.value]);
async function openColumn(item, level) {
  if (!item.directory) { selectEntry(item); return; }
  columnStack.value = columnStack.value.slice(0, level);
  const column = reactive({ prefix: item.key, entries: [], loading: true });
  columnStack.value.push(column);
  const scope = current();
  try {
    const page = await cloudObjects.list({ connectionId: scope.connectionId, prefix: item.key, next: null });
    if (!valid(scope)) return;
    column.entries = objectChildren(page.objects, item.key);
  } catch (cause) { fail(cause, scope); }
  finally { column.loading = false; }
}
const galleryEntries = computed(() => children.value.filter((item) => !item.directory && fileKind({ isDir: false, name: item.name }) === "image"));
// objectChildren keeps the trailing slash on folder names; the file views trim
// it, and FileGlyph/fileKind speak `isDir` rather than `directory`.
function displayName(item) { return item.directory ? item.name.replace(/\/$/, "") : item.name; }
function glyphEntry(item) { return { isDir: item.directory, name: item.name }; }
let catalogVersion = 0;
const drives = computed(() => catalogDrives.value.map((item, index) => ({ ...item, label: t("drive.number", { number: index + 1 }) })));
function openManagement(id = "") { activeDriveId.value = id; view.value = "management"; }
// Selecting a bucket is like opening a drive: the session is rebound to that
// bucket and browsing restarts at its root.
async function selectBucket(bucket) {
  if (busy.value || loading.value) return;
  activeBucket.value = bucket; activeDriveId.value = ""; view.value = "files";
  requestId++; prefix.value = ""; root.value = ""; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false; partial.value = null; error.value = null; prepared.value = false; preparationError.value = null; selectedEntry.value = null; columnStack.value = [];
  const scope = current();
  try { await cloudObjects.selectBucket(props.connectionId, bucket); }
  catch (cause) { fail(cause, scope); return; }
  if (!valid(scope) || disposed) return;
  void prepare();
  void load();
}
async function refreshCatalog() {
  const connectionId = props.connectionId;
  const version = ++catalogVersion;
  try {
    const catalog = await cloud.list();
    if (disposed || version !== catalogVersion || connectionId !== props.connectionId) return;
    connections.value = catalog.connections;
    const connection = (catalog.connections || []).find((item) => String(item.id) === connectionId);
    const list = connection?.buckets?.length ? connection.buckets : (connection?.bucket ? [connection.bucket] : []);
    buckets.value = list;
    if (!activeBucket.value && list.length) activeBucket.value = list[0];
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
const connectionHost = computed(() => {
  const item = connections.value.find((candidate) => String(candidate.id) === props.connectionId);
  const endpoint = item?.endpoint || "";
  try { return new URL(endpoint).host || endpoint; } catch { return endpoint; }
});
const visibleError = computed(() => preparationError.value || error.value);
const failureStrategy = computed(() => visibleError.value ? errorStrategy(visibleError.value) : null);
const transfers = ref([]);
const partial = ref(null);
// PeerBrowser-style selection: the file row only selects; download and delete
// live in the bottom action bar so rows stay readable.
const selectedEntry = ref(null);
function selectEntry(item) { selectedEntry.value = item; }
// The titlebar only exists in the app shell; without it (unit tests) the
// Teleport stays disabled so the actions render in place.
const titlebarActions = ref(false);
let disposed = false;
let requestId = 0;
let statusVersion = 0;
let polling = false;
let timer;
const children = computed(() => objectChildren(objects.value, prefix.value));
// PeerBrowser-style path: the bucket is the root crumb and every level is
// clickable, so there is no separate "up" control.
const breadcrumbs = computed(() => {
  const crumbs = [{ name: activeBucket.value || connectionName.value, path: root.value }];
  let path = root.value;
  for (const segment of prefix.value.slice(root.value.length).split("/").filter(Boolean)) {
    path += `${segment}/`;
    crumbs.push({ name: segment, path });
  }
  return crumbs;
});
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
  protectedRoot.value = false; busy.value = false; partial.value = null; selectedEntry.value = null; columnStack.value = [];
  void load();
}
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
    app.setNotice("cloudBrowser.uploaded", { key });
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
  view.value = "files"; activeDriveId.value = ""; catalogDrives.value = []; buckets.value = []; activeBucket.value = "";
  objects.value = []; transfers.value = []; partial.value = null; selectedEntry.value = null; columnStack.value = []; prefix.value = ""; root.value = ""; next.value = null; complete.value = false; busy.value = false; loading.value = false; error.value = null;
  void prepare();
  void load();
  void refreshCatalog();
}, { immediate: true });
onMounted(() => { void poll(); timer = setInterval(poll, 1500); titlebarActions.value = Boolean(typeof document !== "undefined" && document.getElementById("titlebar-file-actions")); });
onBeforeUnmount(() => { disposed = true; requestId++; statusVersion++; clearInterval(timer); });
</script>

<template>
  <section class="cloud-browser" :aria-label="t('cloudBrowser.title')">
    <div class="browser-body">
      <aside class="browser-sidebar">
        <div class="sidebar-identity"><strong>{{ connectionName }}</strong><span class="mono">{{ connectionHost }}</span></div>
        <p class="sidebar-label">{{ t('cloudBrowser.buckets') }}</p>
        <button v-for="bucket in buckets" :key="bucket" type="button" class="share-link bucket-link" :class="{ 'is-active': view === 'files' && activeBucket === bucket }" @click="selectBucket(bucket)">
          <FileGlyph :entry="{ isDir: true, name: bucket }" /><span>{{ bucket }}</span>
        </button>
        <p v-if="!buckets.length" class="sidebar-empty">{{ t('cloudBrowser.noBuckets') }}</p>
        <div class="sidebar-section-head">
          <p class="sidebar-label">{{ t('cloudBrowser.syncDrives') }}</p>
          <button type="button" class="add-sync-drive" :aria-label="t('drive.addFolder')" @click="openManagement()"><span aria-hidden="true">+</span></button>
        </div>
        <button v-for="(drive, index) in drives" :key="index" type="button" class="share-link drive-link" :class="{ 'is-active': view === 'management' && activeDriveId === drive.id }" @click="openManagement(drive.id)">
          <FileGlyph :entry="{ isDir: true, name: drive.label }" /><span>{{ drive.label }}</span>
        </button>
        <p v-if="!drives.length" class="sidebar-empty">{{ t('cloudBrowser.noSyncDrives') }}</p>
      </aside>
      <main class="browser-main">
        <div v-if="view === 'management'" class="management-view">
          <header class="cloud-toolbar"><el-button class="back-to-files" @click="view = 'files'; activeDriveId = ''">{{ t('cloudBrowser.allFiles') }}</el-button><h2>{{ t('drive.manage') }}</h2></header>
          <div class="management-content"><CloudSyncDrives :key="connectionId" :connection-id="connectionId" :focus-id="activeDriveId" @changed="refreshCatalog" /></div>
        </div>
        <div v-else class="files-view">
          <Teleport to="#titlebar-file-actions" :disabled="!titlebarActions">
            <div class="pathbar-actions">
              <div class="view-switch" role="group" :aria-label="t('cloudBrowser.viewMode')">
                <el-tooltip v-for="mode in VIEW_MODES" :key="mode" :content="t(`cloudBrowser.${mode}`)" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" :class="{ 'is-active': viewMode === mode }" :aria-label="t(`cloudBrowser.${mode}`)" :aria-pressed="viewMode === mode" @click="setViewMode(mode)">
                    <svg v-if="mode === 'icons'" viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="4" width="6" height="6" rx="1" /><rect x="14" y="4" width="6" height="6" rx="1" /><rect x="4" y="14" width="6" height="6" rx="1" /><rect x="14" y="14" width="6" height="6" rx="1" /></svg>
                    <svg v-else-if="mode === 'list'" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16" /></svg>
                    <svg v-else-if="mode === 'columns'" viewBox="0 0 24 24" aria-hidden="true"><rect x="3.5" y="4.5" width="5" height="15" rx="1" /><rect x="9.5" y="4.5" width="5" height="15" rx="1" /><rect x="15.5" y="4.5" width="5" height="15" rx="1" /></svg>
                    <svg v-else viewBox="0 0 24 24" aria-hidden="true"><rect x="3.5" y="4.5" width="17" height="11" rx="1.5" /><rect x="5.5" y="17.5" width="4" height="3" rx="0.8" /><rect x="10.5" y="17.5" width="4" height="3" rx="0.8" /><rect x="15.5" y="17.5" width="3" height="3" rx="0.8" /></svg>
                  </button>
                </el-tooltip>
              </div>
              <span class="titlebar-divider" aria-hidden="true"></span>
              <div class="view-switch" role="group" :aria-label="t('cloudBrowser.fileActions')">
                <el-tooltip :content="t('cloudBrowser.refresh')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" data-test="refresh" :aria-label="t('cloudBrowser.refresh')" :disabled="busy || loading" @click="navigate(prefix)">
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 11a8 8 0 1 0-2.3 5.7" /><path d="M20 5v6h-6" /></svg>
                  </button>
                </el-tooltip>
                <el-tooltip :content="t('cloudBrowser.upload')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" data-test="upload" :aria-label="t('cloudBrowser.upload')" :disabled="!canMutate" @click="upload">
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 16V4" /><path d="m8 8 4-4 4 4" /><path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" /></svg>
                  </button>
                </el-tooltip>
                <el-tooltip :content="t('cloudBrowser.deletePrefix')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" data-test="delete-prefix" :aria-label="t('cloudBrowser.deletePrefix')" :disabled="!canMutate || hasProtectedObjects" @click="remove(prefix)">
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16" /><path d="M9 7V5h6v2" /><path d="M6 7l1 12h10l1-12" /></svg>
                  </button>
                </el-tooltip>
              </div>
            </div>
          </Teleport>
    <div class="pathbar">
      <div class="breadcrumbs" :aria-label="t('cloudBrowser.directory')">
        <template v-for="(crumb, index) in breadcrumbs" :key="crumb.path">
          <span v-if="index" class="breadcrumb-sep" aria-hidden="true">›</span>
          <button type="button" class="breadcrumb" :class="{ 'is-current': index === breadcrumbs.length - 1 }" @click="navigate(crumb.path)">{{ crumb.name }}</button>
        </template>
      </div>
      <p v-if="preparing" class="pathbar-status" role="status">{{ t('cloudBrowser.checking') }}</p>
    </div>
    <div v-if="visibleError" role="alert" class="cloud-error" :data-error-strategy="failureStrategy">
      <span class="cloud-error__message">{{ visibleError.message }}</span>
      <el-button v-if="!preparing && preparationError" data-test="recheck" size="small" :disabled="busy" @click="prepare">{{ t('cloudBrowser.recheck') }}</el-button>
    </div>
    <p v-if="loading" role="status">{{ t('cloudBrowser.loading') }}</p>
    <p v-if="!complete" data-test="incomplete">{{ t('cloudBrowser.incomplete') }}</p>
    <p v-if="!loading && complete && !children.length">{{ t('cloudBrowser.empty') }}</p>
    <div v-if="viewMode === 'icons'" class="icon-grid">
      <button v-for="item in children" :key="item.key" type="button" class="file-tile" :class="{ 'is-selected': selectedEntry?.key === item.key }" :title="displayName(item)" @click="item.directory ? navigate(item.key) : selectEntry(item)">
        <FileGlyph :entry="glyphEntry(item)" size="lg" />
        <span class="file-tile__name">{{ displayName(item) }}</span>
      </button>
    </div>
    <div v-else-if="viewMode === 'columns'" class="column-view">
      <div v-for="(column, index) in columns" :key="column.prefix" class="column">
        <button v-for="item in column.entries" :key="item.key" type="button" class="column-row" :class="{ 'is-selected': selectedEntry?.key === item.key }" :title="displayName(item)" @click="openColumn(item, index)">
          <FileGlyph :entry="glyphEntry(item)" />
          <span class="column-name">{{ displayName(item) }}</span>
          <span v-if="item.directory" class="column-chevron" aria-hidden="true">›</span>
        </button>
        <p v-if="!column.entries.length && !column.loading" class="column-empty">{{ t('cloudBrowser.empty') }}</p>
        <p v-if="column.loading" class="column-empty" role="status">{{ t('cloudBrowser.loading') }}</p>
      </div>
    </div>
    <div v-else-if="viewMode === 'gallery'" class="gallery-view">
      <button v-for="item in galleryEntries" :key="item.key" type="button" class="gallery-tile" :class="{ 'is-selected': selectedEntry?.key === item.key }" :title="displayName(item)" @click="selectEntry(item)">
        <FileGlyph :entry="glyphEntry(item)" size="lg" />
        <span class="gallery-tile__name">{{ displayName(item) }}</span>
      </button>
      <p v-if="!galleryEntries.length" class="gallery-empty">{{ t('cloudBrowser.galleryEmpty') }}</p>
    </div>
    <ul v-else class="cloud-tree" :aria-label="t('cloudBrowser.directory')">
      <li v-for="item in children" :key="item.key" :class="{ 'is-selected': selectedEntry?.key === item.key }">
        <el-button v-if="item.directory" class="folder-link" @click="navigate(item.key)">{{ item.name }}</el-button>
        <button v-else type="button" class="file-row" data-test="file-row" :class="{ 'is-selected': selectedEntry?.key === item.key }" :aria-pressed="selectedEntry?.key === item.key" @click="selectEntry(item)">
          <FileGlyph :entry="glyphEntry(item)" />
          <span class="file-row__name">{{ item.name }}</span>
          <span class="file-row__size">{{ t('cloudBrowser.bytes', { size: item.size }) }}</span>
          <span v-if="item.protected" class="file-row__protected">{{ t('cloudBrowser.protected') }}</span>
        </button>
      </li>
    </ul>
    <el-button v-if="next" data-test="more" :disabled="loading" @click="load(true)">{{ t('cloudBrowser.more') }}</el-button>
    <div v-if="selectedEntry" class="selection-bar">
      <div class="selection-summary"><FileGlyph :entry="{ isDir: false, name: selectedEntry.name }" /><div><strong>{{ selectedEntry.name }}</strong><span>{{ t('cloudBrowser.bytes', { size: selectedEntry.size }) }}</span></div></div>
      <div class="selection-actions">
        <el-button data-test="download" plain :disabled="busy" @click="download(selectedEntry)">{{ t('cloudBrowser.download') }}</el-button>
        <el-button data-test="delete-file" type="danger" plain :disabled="!canMutate || targetProtected(selectedEntry.key)" @click="remove(selectedEntry.key)">{{ t('cloudBrowser.delete') }}</el-button>
      </div>
    </div>
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
.sidebar-section-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px 8px 0; }
.sidebar-section-head .sidebar-label { padding: 0; }
.add-sync-drive { flex: 0 0 auto; display: inline-flex; align-items: center; justify-content: center; width: 22px; height: 22px; padding: 0; border: 1px solid var(--app-border); border-radius: 6px; background: transparent; color: var(--app-text-secondary); font-size: 14px; line-height: 1; cursor: pointer; }
.add-sync-drive:hover { background: var(--app-surface-3); color: var(--app-text); }
.share-link { display: flex; align-items: center; gap: 8px; width: 100%; min-width: 0; padding: 8px; border: 0; border-radius: 6px; background: transparent; color: var(--app-text); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.cloud-browser .share-link span, .cloud-browser .sidebar-identity span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.share-link :deep(.file-glyph) { flex: 0 0 18px; width: 18px; height: 18px; }
.share-link:hover, .share-link.is-active { background: var(--app-surface-3); }
.share-link.is-active { box-shadow: inset 3px 0 var(--el-color-primary); }
.browser-main { min-width: 0; min-height: 0; display: flex; overflow: hidden; }
.files-view, .management-view { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 12px; padding: 16px; overflow: hidden; }
.files-view { overflow-y: auto; }
.files-view > :not(.cloud-tree):not(.icon-grid):not(.column-view):not(.gallery-view) { flex-shrink: 0; }
.files-view > .cloud-tree, .files-view > .icon-grid, .files-view > .column-view, .files-view > .gallery-view { flex: 1 1 auto; min-height: 0; overflow: auto; }
.icon-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(96px, 1fr)); gap: 4px; padding: 4px; align-content: start; }
.file-tile { display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 12px 6px; border: 0; border-radius: 8px; background: transparent; color: var(--app-text); font: inherit; font-size: 12px; text-align: center; cursor: pointer; }
.file-tile:hover, .file-tile.is-selected { background: var(--app-surface-3); }
.file-tile__name { width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.column-view { display: flex; align-items: stretch; gap: 1px; min-width: 0; background: var(--app-border); }
.column { flex: 0 0 200px; min-width: 0; overflow-y: auto; background: var(--app-surface); padding: 6px; display: flex; flex-direction: column; gap: 2px; }
.column-row { display: flex; align-items: center; gap: 8px; width: 100%; padding: 7px 8px; border: 0; border-radius: 6px; background: transparent; color: var(--app-text); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.column-row:hover, .column-row.is-selected { background: var(--app-surface-3); }
.column-name { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.column-chevron { flex: none; color: var(--app-text-muted); }
.column-empty { margin: 8px; color: var(--app-text-muted); font-size: 12px; }
.gallery-view { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 8px; padding: 4px; align-content: start; }
.gallery-tile { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 18px 8px; border: 1px solid var(--app-border); border-radius: 10px; background: var(--app-surface-2); color: var(--app-text); font: inherit; font-size: 12px; text-align: center; cursor: pointer; }
.gallery-tile:hover, .gallery-tile.is-selected { background: var(--app-surface-3); border-color: var(--el-color-primary); }
.gallery-tile__name { width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.gallery-empty { color: var(--app-text-muted); font-size: 12.5px; }
.titlebar-divider { flex: none; width: 1px; height: 16px; background: var(--app-border); }
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
.cloud-toolbar { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; min-width: 0; }
.cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 240px; max-width: 100%; min-width: 0; }
.cloud-tree { margin: 0; padding: 0; list-style: none; }
.cloud-tree li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px; border-bottom: 1px solid var(--app-border); }
.cloud-tree li.is-selected { background: var(--app-surface-3); }
.pathbar { flex-shrink: 0; display: flex; align-items: center; gap: 12px; padding-bottom: 10px; border-bottom: 1px solid var(--app-border); }
.breadcrumbs { flex: 1; min-width: 0; display: flex; align-items: center; gap: 2px; overflow-x: auto; overflow-y: hidden; white-space: nowrap; scrollbar-width: none; }
.breadcrumbs::-webkit-scrollbar { display: none; }
.breadcrumb { flex: none; border: 0; border-radius: 6px; padding: 4px 6px; background: transparent; color: var(--app-text-muted); font: inherit; font-size: 12.5px; cursor: pointer; }
.breadcrumb:hover { color: var(--app-text); background: var(--app-surface-2); }
.breadcrumb.is-current { color: var(--app-text); font-weight: 600; }
.breadcrumb-sep { flex: none; color: var(--app-text-muted); font-size: 14px; }
.pathbar-status { flex: none; font-size: 12px; }
.cloud-error { flex-shrink: 0; display: flex; align-items: center; gap: 10px; color: var(--el-color-danger); }
.file-row { flex: 1 1 auto; display: flex; align-items: center; gap: 8px; min-width: 0; padding: 0; border: 0; background: transparent; color: inherit; font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.file-row__name { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.file-row__size, .file-row__protected { flex: 0 0 auto; color: var(--app-text-muted); }
.file-row.is-selected .file-row__name { color: var(--el-color-primary); font-weight: 600; }
.selection-bar { flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border: 1px solid var(--app-border); border-radius: 8px; background: var(--app-surface-2); }
.selection-summary { display: flex; align-items: center; gap: 10px; min-width: 0; }
.selection-summary div { display: flex; flex-direction: column; min-width: 0; }
.selection-summary strong, .selection-summary span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.selection-summary span { font-size: 12px; color: var(--app-text-muted); }
.selection-actions { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
.pathbar-actions { display: flex; align-items: center; gap: 8px; }
.view-switch { display: flex; align-items: center; gap: 4px; }
.view-switch__button { display: inline-flex; align-items: center; justify-content: center; width: 26px; height: 26px; padding: 0; border: 0; border-radius: 6px; background: transparent; color: var(--app-text-secondary); cursor: pointer; }
.view-switch__button:hover:not(:disabled) { background: var(--app-surface-3); color: var(--app-text); }
.view-switch__button.is-active { background: var(--app-surface-3); color: var(--el-color-primary); }
.view-switch__button:disabled { opacity: 0.45; cursor: not-allowed; }
.view-switch__button svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
.cloud-browser span, .cloud-browser code, .cloud-browser [role=alert], .cloud-browser li { min-width: 0; overflow-wrap: anywhere; white-space: normal; }
.cloud-browser :deep(.el-button + .el-button) { margin-left: 0; }
.cloud-browser [role=alert] { color: var(--el-color-danger); }
@media (max-width: 480px) { .files-view, .management-view { padding: 10px; } .cloud-tree li { flex-wrap: wrap; align-items: flex-start; } .cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 100%; } .selection-bar { flex-wrap: wrap; } }
</style>
