<script setup>
import { computed, h, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { cloud, cloudObjects, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import FileGlyph from "./FileGlyph.vue";
import FilePreviewDialog from "./FilePreviewDialog.vue";
import { canPreview, fileKind, fileKindKey } from "../lib/fileKinds";
import { formatBytes } from "../lib/format";
import {
  MAX_PREVIEW_BYTES,
  previewFormat as detectPreviewFormat,
} from "../lib/textPreview";
import { errorStrategy, normalizeAppError } from "../lib/invoke";
import CloudSyncDrives from "./CloudSyncDrives.vue";
import { t } from "../lib/i18n";

const props = defineProps({ connectionId: { type: String, required: true } });
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
  if (mode === "gallery" && galleryFile.value && previewKey.value !== galleryFile.value.key && canPreview(galleryFile.value)) {
    void previewEntry(galleryFile.value);
  }
}

// Column view drills into subdirectories without leaving the current folder:
// the first column is always the current directory, the stack holds deeper ones.
const columnStack = ref([]);
const columns = computed(() => [
  { prefix: prefix.value, entries: entries.value, loading: false },
  ...columnStack.value,
]);
async function openColumn(item, level) {
  if (!item.isDir) { selectEntry(item); return; }
  columnStack.value = columnStack.value.slice(0, level);
  const column = reactive({ prefix: item.key, entries: [], loading: true });
  columnStack.value.push(column);
  const scope = current();
  try {
    const page = await cloudObjects.list({ connectionId: scope.connectionId, prefix: item.key, next: null });
    if (!valid(scope)) return;
    column.entries = toEntries(objectChildren(page.objects, item.key));
  } catch (cause) { fail(cause, scope); }
  finally { column.loading = false; }
}

// `objectChildren` keeps the trailing slash on folder names; every view trims
// it, and FileGlyph/fileKind speak `isDir` rather than `directory`.
function displayName(item) {
  return item.directory ? item.name.replace(/\/+$/, "") : item.name;
}
function toEntries(list) {
  return list.map((item) => ({
    key: item.key,
    name: displayName(item),
    isDir: item.directory === true,
    size: item.size,
    etag: item.etag ?? null,
    protected: item.protected === true,
  }));
}
const entries = computed(() => toEntries(children.value));

function isSelected(item) { return selectedEntry.value?.key === item.key; }
function entryKind(item) { return t(fileKindKey(fileKind(item))); }
function formatSize(item) {
  if (!item || item.isDir) return "—";
  return formatBytes(item.size);
}
function glyph(item) {
  return item ?? { isDir: true, name: "" };
}

let catalogVersion = 0;
const drives = computed(() => catalogDrives.value.map((item, index) => ({ ...item, label: t("drive.number", { number: index + 1 }) })));
function openManagement(id = "") {
  closePreview();
  selectedEntry.value = null;
  activeDriveId.value = id;
  view.value = "management";
}
// Selecting a bucket is like opening a drive: the session is rebound to that
// bucket and browsing restarts at its root. The previous listing stays visible
// until the backend confirms the switch, so a failure never blanks the view.
async function selectBucket(bucket) {
  if (busy.value || loading.value) return;
  const previousBucket = activeBucket.value;
  activeBucket.value = bucket;
  activeDriveId.value = "";
  view.value = "files";
  requestId++;
  try { await cloudObjects.selectBucket(props.connectionId, bucket); }
  catch (cause) {
    activeBucket.value = previousBucket;
    app.handleFailure(cause);
    void load();
    return;
  }
  if (disposed) return;
  prefix.value = ""; root.value = ""; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false; partial.value = null; error.value = null; prepared.value = false; preparationError.value = null; selectedEntry.value = null; columnStack.value = [];
  clearPreview();
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

// PeerBrowser-style selection: a row click only selects; a double click opens
// a folder or previews a file. Download and delete live in the bottom bar.
const selectedEntry = ref(null);
function selectEntry(item) {
  selectedEntry.value = item;
  if (viewMode.value === "gallery" && !item.isDir && canPreview(item)
    && (previewKey.value !== item.key || (!previewData.value && !previewLimit.value))) {
    void previewEntry(item);
  }
}
function openEntry(item) {
  if (!item) return;
  if (item.isDir) { navigate(item.key); return; }
  if (canPreview(item)) void previewEntry(item);
}

// Preview state mirrors PeerBrowser: media renders inline in gallery mode,
// everything else opens the shared full-browser preview.
const previewOpen = ref(false);
const previewLimit = ref(null);
const previewData = ref(null);
const previewKey = ref("");
const previewTarget = computed(() => selectedEntry.value);
const activePreview = computed(() => (
  previewData.value && previewKey.value === selectedEntry.value?.key ? previewData.value : null
));
const previewSource = computed(() => {
  if (!activePreview.value || activePreview.value.encoding !== "base64") return "";
  return `data:${activePreview.value.mime};base64,${activePreview.value.data}`;
});
const isImagePreview = computed(() => Boolean(activePreview.value?.mime?.startsWith("image/")));
const isAudioPreview = computed(() => Boolean(activePreview.value?.mime?.startsWith("audio/")));
const isVideoPreview = computed(() => Boolean(activePreview.value?.mime?.startsWith("video/")));
const galleryFile = computed(() => (
  selectedEntry.value && !selectedEntry.value.isDir ? selectedEntry.value : null
));

function previewUsesDialog(entry, preview) {
  if (viewMode.value !== "gallery") return true;
  const mime = String(preview?.mime ?? "");
  if (mime.startsWith("image/") || mime.startsWith("audio/") || mime.startsWith("video/")) {
    return false;
  }
  return ["markdown", "table", "code", "text"].includes(detectPreviewFormat(entry));
}
function closePreview() {
  previewOpen.value = false;
  previewLimit.value = null;
}
function clearPreview() {
  closePreview();
  previewData.value = null;
  previewKey.value = "";
}
async function previewEntry(entry) {
  if (!entry || entry.isDir || busy.value || !canPreview(entry)) return;
  selectedEntry.value = entry;
  const key = entry.key;
  previewKey.value = key;
  if (Number(entry.size) > MAX_PREVIEW_BYTES) {
    previewData.value = null;
    previewLimit.value = {
      code: "PEER_PREVIEW_TOO_LARGE",
      size: Number(entry.size),
      limit: MAX_PREVIEW_BYTES,
    };
    previewOpen.value = true;
    return;
  }
  previewLimit.value = null;
  previewData.value = null;
  await operate(async (scope) => {
    try {
      const preview = await cloudObjects.preview({ connectionId: scope.connectionId, key });
      if (!valid(scope) || selectedEntry.value?.key !== key) return;
      previewData.value = preview;
      previewKey.value = key;
      previewOpen.value = previewUsesDialog(entry, preview);
    } catch (cause) {
      if (cause?.code === "PEER_PREVIEW_TOO_LARGE" || cause?.code === "PEER_PREVIEW_TOO_MANY_LINES") {
        if (!valid(scope)) return;
        previewKey.value = key;
        previewLimit.value = { code: cause.code, ...(cause.context ?? {}) };
        previewOpen.value = true;
        return;
      }
      fail(cause, scope);
    }
  });
}

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
  clearPreview();
  void load();
}
// Refresh keeps the current listing on screen until the new page arrives, so a
// transient network failure never looks like the folder became empty.
function refresh() {
  requestId++;
  next.value = null; complete.value = false; busy.value = false; partial.value = null;
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
  if (!item || item.isDir) return;
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
  clearPreview();
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
                  <button type="button" class="view-switch__button" data-test="refresh" :aria-label="t('cloudBrowser.refresh')" :disabled="busy || loading" @click="refresh">
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
            <el-button v-else-if="error" data-test="retry-load" size="small" :disabled="loading" @click="refresh">{{ t('cloudBrowser.retry') }}</el-button>
          </div>
          <p v-if="loading" role="status">{{ t('cloudBrowser.loading') }}</p>
          <p v-if="!complete" data-test="incomplete">{{ t('cloudBrowser.incomplete') }}</p>
          <p v-if="!loading && complete && !entries.length">{{ t('cloudBrowser.empty') }}</p>

          <div v-if="viewMode === 'icons'" class="icon-grid">
            <button v-for="item in entries" :key="item.key" type="button" class="file-tile" :class="{ 'is-selected': isSelected(item) }" :title="item.name" @click="selectEntry(item)" @dblclick="openEntry(item)">
              <FileGlyph :entry="item" size="lg" />
              <span class="file-tile__name">{{ item.name }}</span>
            </button>
          </div>

          <div v-else-if="viewMode === 'list'" class="file-table-wrap">
            <table class="file-table">
              <thead>
                <tr>
                  <th scope="col">{{ t('peer.name') }}</th>
                  <th scope="col">{{ t('peer.kind') }}</th>
                  <th scope="col">{{ t('peer.size') }}</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="item in entries"
                  :key="item.key"
                  tabindex="0"
                  :data-test="item.isDir ? 'folder-row' : 'file-row'"
                  :class="{ 'is-selected': isSelected(item) }"
                  @click="selectEntry(item)"
                  @dblclick="openEntry(item)"
                  @keydown.enter="openEntry(item)"
                >
                  <td>
                    <span class="table-name">
                      <FileGlyph :entry="item" />
                      <span>{{ item.name }}</span>
                      <span v-if="item.protected" class="table-protected">{{ t('cloudBrowser.protected') }}</span>
                    </span>
                  </td>
                  <td>{{ entryKind(item) }}</td>
                  <td>{{ formatSize(item) }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-else-if="viewMode === 'columns'" class="column-view">
            <div v-for="(column, index) in columns" :key="column.prefix" class="column">
              <button v-for="item in column.entries" :key="item.key" type="button" class="column-row" :class="{ 'is-selected': isSelected(item) }" :title="item.name" @click="openColumn(item, index)" @dblclick="openEntry(item)">
                <FileGlyph :entry="item" />
                <span class="column-name">{{ item.name }}</span>
                <span v-if="item.isDir" class="column-chevron" aria-hidden="true">›</span>
              </button>
              <p v-if="!column.entries.length && column.loading" class="column-empty" role="status">{{ t('cloudBrowser.loading') }}</p>
              <p v-else-if="!column.entries.length" class="column-empty">{{ t('peer.emptyDir') }}</p>
            </div>
          </div>

          <div v-else class="gallery-view">
            <div class="gallery-stage">
              <template v-if="galleryFile">
                <img v-if="isImagePreview && previewSource" :src="previewSource" :alt="t('peer.previewAlt')" class="gallery-image" />
                <audio v-else-if="isAudioPreview && previewSource" :src="previewSource" class="gallery-audio" controls preload="metadata"></audio>
                <video v-else-if="isVideoPreview && previewSource" :src="previewSource" class="gallery-video" controls playsinline preload="metadata"></video>
                <pre v-else-if="activePreview && activePreview.encoding === 'utf8'" class="gallery-text">{{ activePreview.data }}</pre>
                <div v-else class="gallery-placeholder">
                  <FileGlyph :entry="galleryFile" size="xl" />
                  <strong>{{ galleryFile.name }}</strong>
                  <span>{{ entryKind(galleryFile) }} · {{ formatSize(galleryFile) }}</span>
                  <el-button v-if="canPreview(galleryFile)" type="primary" plain :disabled="busy" @click="previewEntry(galleryFile)">
                    {{ t('peer.preview') }}
                  </el-button>
                </div>
              </template>
              <div v-else class="gallery-placeholder">
                <FileGlyph :entry="glyph(selectedEntry)" size="xl" />
                <strong>{{ selectedEntry ? selectedEntry.name : t('peer.selectPreview') }}</strong>
                <span>{{ selectedEntry ? entryKind(selectedEntry) : t('peer.selectPreviewHint') }}</span>
              </div>
            </div>
            <div class="gallery-strip">
              <button v-for="item in entries" :key="item.key" type="button" class="gallery-thumb" :class="{ 'is-selected': isSelected(item) }" :title="item.name" @click="selectEntry(item)" @dblclick="openEntry(item)">
                <FileGlyph :entry="item" />
                <span>{{ item.name }}</span>
              </button>
            </div>
          </div>

          <el-button v-if="next" data-test="more" class="load-more" :disabled="loading" @click="load(true)">{{ t('cloudBrowser.more') }}</el-button>
          <div v-if="selectedEntry" class="selection-bar">
            <div class="selection-summary">
              <FileGlyph :entry="selectedEntry" />
              <div>
                <strong>{{ selectedEntry.name }}</strong>
                <span>{{ selectedEntry.isDir ? t('peer.folder') : formatBytes(selectedEntry.size) }}<template v-if="selectedEntry.protected"> · {{ t('cloudBrowser.protected') }}</template></span>
              </div>
            </div>
            <div class="selection-actions">
              <el-button v-if="!selectedEntry.isDir" data-test="download" plain :disabled="busy" @click="download(selectedEntry)">{{ t('cloudBrowser.download') }}</el-button>
              <el-button data-test="delete-file" type="danger" plain :disabled="!canMutate || targetProtected(selectedEntry.key)" @click="remove(selectedEntry.key)">{{ t('cloudBrowser.delete') }}</el-button>
            </div>
          </div>
          <div v-if="partial" role="status"><p>{{ t('cloudBrowser.deleteResult', { deleted: partial.deleted.length, failed: partial.failed.length }) }}</p><ul><li v-for="item in partial.failed" :key="item.key">{{ item.key }} · {{ item.error.message }}</li></ul></div>
          <h3 v-if="transfers.length">{{ t('cloudBrowser.transfers') }}</h3>
          <ul v-if="transfers.length" class="cloud-tree cloud-transfers"><li v-for="item in transfers" :key="item.transferId"><span>{{ item.key == null ? t('cloudBrowser.unknownTarget') : JSON.stringify(item.key) }}<span v-if="item.sizeHint != null"> · {{ t('cloudBrowser.bytes', { size: item.sizeHint }) }}</span> · {{ t(`cloudBrowser.${item.direction}`) }} · {{ t(`cloudBrowser.phase.${item.phase}`) }}<strong v-if="item.recoveryRequired || item.phase === 'unknown'"> · {{ t('cloudBrowser.recovery') }}</strong><span v-if="item.error"> · {{ item.error.message }}</span></span><el-button v-if="item.phase === 'failed' && item.direction === 'download' && item.error?.code === 'CLOUD_PRECONDITION' && downloadTargets[item.transferId]" :disabled="busy" @click="retryDownload(downloadTargets[item.transferId])">{{ t('cloudBrowser.retryDownload') }}</el-button><p v-if="item.phase === 'acknowledged'">{{ t('cloudBrowser.acknowledgedNotice') }}</p><el-button v-if="item.phase === 'unknown'" :disabled="busy" @click="acknowledge(item)">{{ t('cloudBrowser.acknowledge') }}</el-button><el-button v-if="item.phase === 'running'" :disabled="busy" @click="cancel(item)">{{ t('cloudBrowser.cancel') }}</el-button></li></ul>
        </div>
      </main>
    </div>
    <FilePreviewDialog
      :open="previewOpen"
      :target="previewTarget"
      :preview="activePreview"
      :limit="previewLimit"
      :busy="busy"
      @close="closePreview"
      @download="download(previewTarget)"
    />
  </section>
</template>

<style scoped>
.cloud-browser { position: relative; flex: 1; min-width: 0; min-height: 0; overflow: hidden; display: flex; color: var(--app-text); }
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
.files-view > :not(.file-table-wrap):not(.icon-grid):not(.column-view):not(.gallery-view):not(.cloud-transfers) { flex-shrink: 0; }
.files-view > .file-table-wrap, .files-view > .icon-grid, .files-view > .column-view, .files-view > .gallery-view { flex: 1 1 auto; min-height: 0; }
.files-view > .cloud-transfers { flex: 0 1 30%; }
.load-more { flex: none; align-self: flex-start; }
.management-content { flex: 1; min-height: 0; overflow: auto; }
.pathbar { flex-shrink: 0; display: flex; align-items: center; gap: 12px; padding-bottom: 10px; border-bottom: 1px solid var(--app-border); }
.breadcrumbs { flex: 1; min-width: 0; display: flex; align-items: center; gap: 2px; overflow-x: auto; overflow-y: hidden; white-space: nowrap; scrollbar-width: none; }
.breadcrumbs::-webkit-scrollbar { display: none; }
.breadcrumb { flex: none; border: 0; border-radius: 6px; padding: 4px 6px; background: transparent; color: var(--app-text-muted); font: inherit; font-size: 12.5px; cursor: pointer; }
.breadcrumb:hover { color: var(--app-text); background: var(--app-surface-2); }
.breadcrumb.is-current { color: var(--app-text); font-weight: 600; }
.breadcrumb-sep { flex: none; color: var(--app-text-muted); font-size: 14px; }
.pathbar-status { flex: none; font-size: 12px; }
.cloud-error { flex-shrink: 0; display: flex; align-items: center; gap: 10px; color: var(--el-color-danger); }
.titlebar-divider { flex: none; width: 1px; height: 16px; background: var(--app-border); }

/* File views follow PeerBrowser so folders feel the same on both sides. */
.icon-grid { overflow: auto; align-content: start; display: grid; grid-template-columns: repeat(auto-fill, minmax(112px, 1fr)); gap: 10px; }
.file-tile { min-height: 106px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 5px; padding: 12px 8px; border: 1px solid transparent; border-radius: 10px; background: transparent; color: var(--app-text); font: inherit; cursor: pointer; }
.file-tile:hover { background: var(--app-surface-2); }
.file-tile.is-selected { border-color: var(--selection-border); background: var(--selection-bg); }
.file-tile__name { display: -webkit-box; max-width: 100%; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; overflow-wrap: break-word; text-align: center; font-size: 12.5px; font-weight: 500; line-height: 1.35; }
.file-table-wrap { overflow: auto; }
.file-table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
.file-table th { position: sticky; top: 0; z-index: 2; padding: 7px 9px; border-bottom: 1px solid var(--app-border); background: var(--app-surface); color: var(--app-text-muted); font-size: 11.5px; font-weight: 600; text-align: left; }
.file-table td { padding: 7px 9px; border-bottom: 1px solid color-mix(in oklab, var(--app-border) 55%, transparent); color: var(--app-text-secondary); white-space: nowrap; }
.file-table tbody tr:last-child td { border-bottom: none; }
.file-table tbody tr { cursor: default; }
.file-table tbody tr:hover { background: var(--app-surface-2); }
.file-table tbody tr.is-selected { background: var(--selection-bg); }
.file-table tbody tr.is-selected td { border-bottom-color: color-mix(in oklab, var(--selection-border) 40%, transparent); }
.file-table tbody tr.is-selected:last-child td { border-bottom: none; }
.table-name { display: flex; align-items: center; gap: 8px; color: var(--app-text); }
.table-protected { color: var(--app-text-muted); font-size: 11px; }
.column-view { display: flex; align-items: stretch; overflow-x: auto; overflow-y: hidden; }
.column { width: 240px; flex: none; overflow-y: auto; padding: 5px; border-right: 1px solid var(--app-border); }
.column:last-child { border-right: 0; }
.column-row { width: 100%; display: flex; align-items: center; gap: 7px; padding: 6px 8px; border: 0; border-radius: 7px; background: transparent; color: var(--app-text-secondary); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; }
.column-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.column-row:hover { background: var(--app-surface-2); }
.column-row.is-selected { color: var(--app-text); background: var(--selection-bg); }
.column-chevron { flex: none; color: var(--app-text-muted); font-size: 15px; }
.column-empty { margin: 8px; font-size: 12px; color: var(--app-text-muted); }
.gallery-view { box-sizing: border-box; height: 100%; min-height: 0; display: flex; flex-direction: column; gap: 10px; }
.gallery-stage { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; padding: 16px; border: 1px solid var(--app-border); border-radius: 12px; background: linear-gradient(45deg, color-mix(in oklab, var(--app-surface-2) 55%, transparent) 25%, transparent 25%, 50%, color-mix(in oklab, var(--app-surface-2) 55%, transparent) 50%, 75%, transparent 75%), var(--app-surface-3); background-size: 16px 16px; }
.gallery-stage > * { max-width: 100%; max-height: 100%; min-width: 0; min-height: 0; }
.gallery-image { max-width: 100%; max-height: 100%; width: auto; height: auto; object-fit: contain; border-radius: 8px; box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16); }
.gallery-audio { width: min(440px, 100%); }
.gallery-video { max-width: 100%; max-height: 100%; width: auto; height: auto; object-fit: contain; border-radius: 8px; background: #000; box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16); }
.gallery-text { margin: 0; box-sizing: border-box; width: 100%; height: 100%; min-height: 0; max-height: none; overflow: auto; padding: 16px; border-radius: 8px; background: var(--app-surface); border: 1px solid var(--app-border); font-size: 12px; line-height: 1.55; white-space: pre-wrap; word-break: break-word; }
.gallery-placeholder { display: flex; flex-direction: column; align-items: center; gap: 7px; color: var(--app-text-muted); text-align: center; }
.gallery-placeholder strong { color: var(--app-text); font-size: 14px; }
.gallery-placeholder span { font-size: 12px; }
.gallery-strip { flex: none; display: flex; gap: 8px; overflow-x: auto; padding: 2px; }
.gallery-thumb { width: 96px; flex: none; display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 7px 5px; border: 1px solid transparent; border-radius: 8px; background: transparent; color: var(--app-text-secondary); font: inherit; cursor: pointer; }
.gallery-thumb span { width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; text-align: center; }
.gallery-thumb:hover { background: var(--app-surface-2); }
.gallery-thumb.is-selected { border-color: var(--selection-border); background: var(--selection-bg); }
.selection-bar { flex: none; box-sizing: border-box; min-height: 54px; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 10px; border: 1px solid var(--app-border); border-radius: 8px; background: var(--app-surface-2); }
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
.cloud-browser span, .cloud-browser code, .cloud-browser [role=alert], .cloud-browser li { min-width: 0; overflow-wrap: anywhere; white-space: normal; }
.cloud-browser :deep(.el-button + .el-button) { margin-left: 0; }
.cloud-browser [role=alert] { color: var(--el-color-danger); }
@media (max-width: 480px) { .files-view, .management-view { padding: 10px; } .cloud-tree li { flex-wrap: wrap; align-items: flex-start; } .cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 100%; } .selection-bar { flex-wrap: wrap; } }
</style>