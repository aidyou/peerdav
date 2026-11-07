<script setup>
import { computed, h, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { cloud, cloudObjects, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import FileGlyph from "./FileGlyph.vue";
import { selectionForClick } from "../lib/fileManagerAdapters";
import FileManagerView from "./FileManagerView.vue";
import BrowserSkeleton from "./BrowserSkeleton.vue";
import FilePreviewDialog from "./FilePreviewDialog.vue";
import UploadBatchDialog from "./UploadBatchDialog.vue";
import TransferQueueDialog from "./TransferQueueDialog.vue";
import OperationErrorDialog from "./OperationErrorDialog.vue";
import { useDownloadQueue } from "../composables/useDownloadQueue";
import { useUploadBatch } from "../composables/useUploadBatch";
import { cloudUploadAdapter } from "../lib/uploadBatchAdapters";
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

// 刷新（webview 重新加载）后保持上次停留的视图，不跳回文件首页。
const SCOPE_STORAGE_KEY = "peerdav.cloudViewScope";
function readViewScope() {
  try {
    const raw = globalThis.localStorage?.getItem(SCOPE_STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch { return null; }
}
const savedScope = readViewScope();
const view = ref(["management", "local"].includes(savedScope?.view) ? savedScope.view : "files");
const activeDriveId = ref(typeof savedScope?.activeDriveId === "string" ? savedScope.activeDriveId : "");
const catalogDrives = ref([]);
const buckets = ref([]);
const activeBucket = ref("");
watch([view, activeDriveId], ([v, id]) => {
  try { globalThis.localStorage?.setItem(SCOPE_STORAGE_KEY, JSON.stringify({ view: v, activeDriveId: id })); } catch { /* 禁用存储不应阻塞视图切换 */ }
});

// View modes mirror the LAN browser: icons, list, columns and gallery.
const VIEW_STORAGE_KEY = "peerdav.cloudView";
const VIEW_MODES = ["icons", "list", "columns", "gallery"];
const viewMode = ref(readViewMode());
function readViewMode() {
  try {
    const stored = globalThis.localStorage?.getItem(VIEW_STORAGE_KEY);
    return VIEW_MODES.includes(stored) ? stored : "icons";
  } catch { return "icons"; }
}
function setViewMode(mode) {
  if (!VIEW_MODES.includes(mode)) return;
  viewMode.value = mode;
  try { globalThis.localStorage?.setItem(VIEW_STORAGE_KEY, mode); } catch { /* a disabled storage must not block switching views */ }
  if (view.value === "local") return;
  if (mode === "columns") void rebuildColumns();
  if (mode === "gallery" && galleryFile.value && previewKey.value !== galleryFile.value.key && canPreview(galleryFile.value)) {
    void previewEntry(galleryFile.value);
  }
}

// Column view mirrors PeerBrowser: the first column starts at the drive root,
// every ancestor of the current folder stays visible, and a single click on a
// folder navigates into it while a file only changes the selection.
const columnAncestors = ref([]);
const columnViewEl = ref(null);
const columns = computed(() => [
  ...columnAncestors.value,
  { prefix: prefix.value, entries: entries.value, loading: loading.value, failed: Boolean(error.value), complete: complete.value },
]);
const columnLoading = computed(() => viewMode.value === "columns" && columns.value.some((column) => column.loading));
let columnVersion = 0;
async function rebuildColumns() {
  const version = ++columnVersion;
  const scope = current();
  const chain = [];
  let path = root.value;
  chain.push(path);
  for (const segment of prefix.value.slice(root.value.length).split("/").filter(Boolean)) {
    path = `${path}${segment}/`;
    chain.push(path);
  }
  // The current folder renders from the live listing, not a stale snapshot.
  chain.pop();
  columnAncestors.value = chain.map((item) => reactive({ prefix: item, entries: [], loading: true, failed: false, complete: false }));
  await Promise.all(columnAncestors.value.map(async (level) => {
    try {
      const page = await cloudObjects.list({ connectionId: scope.connectionId, prefix: level.prefix, next: null });
      if (version !== columnVersion || !valid(scope)) return;
      level.entries = toEntries(objectChildren(page.objects, level.prefix));
      level.complete = page.complete === true && page.next == null;
    } catch (cause) {
      if (version === columnVersion && valid(scope)) { level.failed = true; fail(cause, scope, rebuildColumns); }
    } finally {
      level.loading = false;
    }
  }));
}
async function openColumn(item, event, column) {
  if (!item || event?.detail > 1) return;
  if (hasSelectionModifier(event)) { selectEntry(item, event); return; }
  if (!item.isDir) { selectSingle(item); return; }
  const levels = columns.value;
  const index = levels.findIndex((level) => level.prefix === column.prefix);
  if (index < 0 || !item.key.startsWith(root.value)) return;
  // Retain the clicked column and its ancestors, never refetch them on a click.
  const ancestors = levels.slice(0, index + 1).map((level) => ({ ...level, entries: level.entries.slice() }));
  requestId++; columnVersion++;
  prefix.value = item.key; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false;
  clearSelection(); clearPreview();
  columnAncestors.value = ancestors;
  const scope = current();
  await load();
  if (valid(scope) && !error.value) setSelection([item]);
}

// 分栏是横向排布的，鼠标滚轮只给纵向增量，必须转成横向滚动，否则目录一深左侧列就无法查看；
// 触控板的横向手势（deltaX）交给浏览器原生滚动，不拦截。
function scrollColumnView(event) {
  const element = event.currentTarget;
  if (!element || element.scrollWidth <= element.clientWidth) return;
  if (Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
  event.preventDefault();
  element.scrollLeft += event.deltaY;
}

// `objectChildren` keeps the trailing slash on folder names; every view trims
// it, and FileGlyph/fileKind speak `isDir` rather than `directory`.
function directoryName(name) {
  if (name === "_peerdav_encrypt") return t("cloudBrowser.encryptedDrive");
  if (name === "_peerdav_mirror") return t("cloudBrowser.mirrorDrive");
  return name;
}
function displayName(item) {
  return item.directory ? directoryName(item.name.replace(/\/+$/, "")) : item.name;
}
function toEntries(list) {
  return list.map((item) => ({
    key: item.key,
    name: displayName(item),
    isDir: item.directory === true,
    driveKind: item.directory && item.name.replace(/\/+$/, "") === "_peerdav_encrypt" ? "encrypted-drive"
      : item.directory && item.name.replace(/\/+$/, "") === "_peerdav_mirror" ? "mirror-drive" : null,
    size: item.size,
    modified: item.modified,
    created: item.created,
    etag: item.etag ?? null,
    protected: item.protected === true,
  }));
}
const entries = computed(() => toEntries(children.value));
// A navigation empties the listing; show a mode-aware skeleton instead of a blank pane.
const showLoading = computed(() => viewMode.value !== "columns" && loading.value && !entries.value.length && !visibleError.value);
// 只有确实拿到部分结果时才提示列表不完整；初次加载与加载失败分别由加载态、错误区表达。
const incompleteListing = computed(() => !complete.value && (next.value != null || objects.value.length > 0));

function entryKind(item) { return t(fileKindKey(fileKind(item))); }
function formatSize(item) {
  if (!item || item.isDir) return "—";
  return formatBytes(item.size);
}

function hasValidTime(seconds) {
  return (typeof seconds === "number" || (typeof seconds === "string" && seconds.trim() !== ""))
    && Number.isFinite(Number(seconds)) && Number.isFinite(new Date(Number(seconds) * 1000).getTime());
}
const timeColumn = computed(() => {
  if (entries.value.some((item) => hasValidTime(item.modified))) return "modified";
  if (entries.value.some((item) => hasValidTime(item.created))) return "created";
  return null;
});
function formatModified(seconds) {
  if (!hasValidTime(seconds)) return "—";
  try {
    return new Intl.DateTimeFormat(document.documentElement.lang || undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(Number(seconds) * 1000));
  } catch { return "—"; }
}

let catalogVersion = 0;
const drives = computed(() => catalogDrives.value.map((item, index) => ({ ...item, label: item.localRoot?.split(/[\\/]+/).filter(Boolean).pop() || item.logicalPath || t("drive.number", { number: index + 1 }) })));
const localDrive = computed(() => drives.value.find(item => item.id === activeDriveId.value));
const localFiles = ref([]);
const localPath = ref("");
const localSelection = ref([]);
const localLoading = ref(false);
const localBusy = ref(false);
const localStatusLoading = ref(false);
const localStatusKnown = ref(false);
const localError = ref(null);
const localPreview = ref(null);
const localPreviewHasChanges = computed(() => ['uploads', 'downloads', 'deletes', 'conflicts'].some(key => Number(localPreview.value?.summary?.[key]) > 0));
const localPhase = ref("");
const localRunId = ref(null);
const localRunning = computed(() => ["scanning", "uploading", "publishing", "applying"].includes(localPhase.value));
let localVersion = 0;
let localRequest = 0;
let localPolling = false;
function resetLocalScope() {
  localVersion++; localRequest++;
  localBusy.value = false; localLoading.value = false; localPreview.value = null; localError.value = null;
  localPhase.value = ""; localRunId.value = null;
  localStatusLoading.value = false; localStatusKnown.value = false;
}
function currentLocal(version) { return !disposed && version === localVersion && view.value === "local"; }
const localChildren = (path) => localFiles.value.filter(entry => (entry.path.includes("/") ? entry.path.slice(0, entry.path.lastIndexOf("/")) : "") === path);
const localEntries = computed(() => localChildren(localPath.value));
const localColumns = computed(() => {
  const paths = [""];
  let path = "";
  for (const part of localPath.value.split("/").filter(Boolean)) { path = path ? `${path}/${part}` : part; paths.push(path); }
  return paths.map(path => ({ path, entries: localChildren(path), complete: !localError.value, failed: Boolean(localError.value) }));
});
const localCrumbs = computed(() => {
  const crumbs = [{ name: localDrive.value?.label || "", path: "" }];
  let path = "";
  for (const part of localPath.value.split("/").filter(Boolean)) { path = path ? `${path}/${part}` : part; crumbs.push({ name: part, path }); }
  return crumbs;
});
async function loadLocal() {
  const id = localDrive.value?.mappingId;
  if (!id) return;
  const version = localVersion;
  const request = ++localRequest;
  localLoading.value = true; localError.value = null; localStatusKnown.value = false;
  try {
    const result = await cloud.localEntries(id);
    if (!currentLocal(version) || request !== localRequest) return;
    localFiles.value = result;
    if (localPath.value && !result.some(entry => entry.isDir && entry.path === localPath.value)) localPath.value = "";
    localSelection.value = localSelection.value.map(old => result.find(entry => entry.path === old.path)).filter(Boolean);
    localLoading.value = false;
    void compareLocalStatus(id, version, request);
  } catch (cause) { if (currentLocal(version) && request === localRequest) { localError.value = normalizeAppError(cause); app.handleFailure(cause); } }
  finally { if (currentLocal(version) && request === localRequest) localLoading.value = false; }
}
async function compareLocalStatus(id, version, request) {
  localStatusLoading.value = true;
  try {
    const entries = await cloud.localStatus(id);
    if (!currentLocal(version) || request !== localRequest) return;
    const states = new Map(entries.map(entry => [entry.path, entry]));
    localFiles.value = localFiles.value.map(entry => ({ ...entry, synced: states.get(entry.path)?.synced === true }));
    localStatusKnown.value = true;
  } catch (cause) {
    if (currentLocal(version) && request === localRequest) app.handleFailure(cause);
  } finally {
    if (currentLocal(version) && request === localRequest) localStatusLoading.value = false;
  }
}
async function openLocal(drive) {
  if (drive.mirror) {
    if (activeDriveId.value === drive.id && view.value === 'files') return;
    const connection = props.connectionId;
    if (drive.bucket && drive.bucket !== activeBucket.value && !await selectBucket(drive.bucket)) return;
    if (disposed || connection !== props.connectionId || (drive.bucket && activeBucket.value !== drive.bucket)) return;
    resetLocalScope(); closePreview(); clearSelection();
    activeDriveId.value = drive.id; view.value = 'files';
    root.value = `_peerdav_mirror/${drive.cloudPrefix ? `${drive.cloudPrefix.replace(/\/+$/, '')}/` : ''}`;
    navigate(root.value);
    return;
  }
  if (activeDriveId.value === drive.id && (view.value === "local" || (view.value === "management" && !drive.mappingId))) return;
  if (!drive.mappingId) { openManagement(drive.id); return; }
  closePreview(); clearSelection(); resetLocalScope();
  activeDriveId.value = drive.id; view.value = "local";
  localFiles.value = []; localPath.value = ""; localSelection.value = []; localPreview.value = null; localPhase.value = "";
  void loadLocal();
}
function selectLocal(entry) { localSelection.value = [entry]; }
function activateLocal(entry) { selectLocal(entry); if (entry.isDir) localPath.value = entry.path; }
async function previewLocalSync() {
  const id = localDrive.value?.mappingId;
  if (!id || localBusy.value || localRunning.value) return;
  const version = localVersion;
  localBusy.value = true; localError.value = null; localPreview.value = null;
  if (['failed', 'cancelled'].includes(localPhase.value)) localPhase.value = '';
  try { const result = await cloud.preview(id); if (currentLocal(version)) localPreview.value = { ...result, mappingId: id, checkedAt: new Date().toISOString() }; }
  catch (cause) { if (currentLocal(version)) { localError.value = normalizeAppError(cause); app.handleFailure(cause); } }
  finally { if (currentLocal(version)) localBusy.value = false; }
}
async function applyLocalSync() {
  const plan = localPreview.value;
  if (!plan || !localPreviewHasChanges.value || localBusy.value || localRunning.value) return;
  const version = localVersion;
  localBusy.value = true;
  try {
    const result = await cloud.sync(plan.mappingId, plan.previewId);
    if (currentLocal(version)) { localPreview.value = null; localPhase.value = "scanning"; localRunId.value = result.runId; }
  }
  catch (cause) { if (currentLocal(version)) { localError.value = normalizeAppError(cause); app.handleFailure(cause); } }
  finally { if (currentLocal(version)) localBusy.value = false; }
}
async function pollLocalSync() {
  if (view.value !== "local" || !localDrive.value?.mappingId || localPolling) return;
  const id = localDrive.value.mappingId;
  const version = localVersion;
  localPolling = true;
  try {
    const statuses = await cloud.status();
    if (!currentLocal(version) || localDrive.value?.mappingId !== id) return;
    const status = statuses.find(item => item.mappingId === id);
    if (status?.phase && (status.phase !== localPhase.value || status.runId !== localRunId.value)) {
      localPhase.value = status.phase; localRunId.value = status.runId;
      if (["completed", "failed", "cancelled"].includes(status.phase)) { localPreview.value = null; void loadLocal(); }
    }
  } catch (cause) { if (currentLocal(version)) { localError.value = normalizeAppError(cause); } }
  finally { localPolling = false; }
}
function openManagement(id = "") {
  resetLocalScope();
  closePreview();
  clearSelection();
  activeDriveId.value = id;
  view.value = "management";
}
// Selecting a bucket is like opening a drive: the session is rebound to that
// bucket and browsing restarts at its root. The previous listing stays visible
// until the backend confirms the switch, so a failure never blanks the view.
let bucketSelectionVersion = 0;
async function selectBucket(bucket) {
  if (busy.value || loading.value) return;
  const connectionId = props.connectionId;
  const version = ++bucketSelectionVersion;
  await cancelUploadSession();
  await disposeDownload();
  if (disposed || connectionId !== props.connectionId || version !== bucketSelectionVersion) return false;
  const previousBucket = activeBucket.value;
  resetLocalScope();
  activeBucket.value = bucket;
  activeDriveId.value = "";
  view.value = "files";
  requestId++;
  try { await cloudObjects.selectBucket(connectionId, bucket); }
  catch (cause) {
    if (disposed || connectionId !== props.connectionId || version !== bucketSelectionVersion) return false;
    activeBucket.value = previousBucket;
    app.handleFailure(cause);
    void load();
    return false;
  }
  if (disposed || connectionId !== props.connectionId || version !== bucketSelectionVersion) return false;
  prefix.value = ""; root.value = ""; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false; error.value = null; prepared.value = false; preparationError.value = null;
  clearSelection();
  columnVersion++; columnAncestors.value = [];
  clearPreview();
  void prepare();
  void load();
  return true;
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
      ...(catalog.mappings || []).filter((item) => item.connectionId === connectionId).map(item => {
        const append = (catalog.appendRepositories || []).find(repo => repo.connectionId === connectionId && repo.binding.repoId === item.repositoryId);
        const legacy = (catalog.repositories || []).find(repo => repo.connectionId === connectionId && repo.id === item.repositoryId);
        return { ...item, mappingId: item.id, driveKind: append?.binding?.appendMode === 'authenticated_plain' || legacy?.mode === 'plain' ? 'plain-drive' : 'encrypted-drive' };
      }),
      ...(catalog.repositories || []).filter((item) => item.connectionId === connectionId && !(catalog.mappings || []).some(m => m.repositoryId === item.id)),
      ...(catalog.appendRepositories || []).filter((item) => item.connectionId === connectionId && !(catalog.mappings || []).some(m => m.repositoryId === item.binding.repoId)).map((item) => ({ id: item.binding.repoId })),
      ...(catalog.mirrorMappings || []).filter((item) => item.connectionId === connectionId).map((item) => ({ ...item, mirror: true, driveKind: 'mirror-drive' })),
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
// 快速完成的操作不该闪现提示文案：超过 300ms 才显示，避免文件区被一行文字挤动。
const HINT_DELAY = 300;
const loadingHint = ref(false);
const checkingHint = ref(false);
let loadingHintTimer = 0;
let checkingHintTimer = 0;
watch(loading, (value) => {
  clearTimeout(loadingHintTimer);
  if (!value) { loadingHint.value = false; return; }
  loadingHintTimer = setTimeout(() => { loadingHint.value = true; }, HINT_DELAY);
});
watch(preparing, (value) => {
  clearTimeout(checkingHintTimer);
  if (!value) { checkingHint.value = false; return; }
  checkingHintTimer = setTimeout(() => { checkingHint.value = true; }, HINT_DELAY);
});
const connectionName = computed(() => connections.value.find((item) => String(item.id) === props.connectionId)?.name || t("cloudBrowser.title"));
const connectionHost = computed(() => {
  const item = connections.value.find((candidate) => String(candidate.id) === props.connectionId);
  const endpoint = item?.endpoint || "";
  try { return new URL(endpoint).host || endpoint; } catch { return endpoint; }
});
const visibleError = computed(() => preparationError.value || error.value);
const retryOperation = ref(null);
function closeOperationError() { error.value = null; preparationError.value = null; retryOperation.value = null; }
function retryFailedOperation() {
  const retry = preparationError.value ? prepare : retryOperation.value;
  closeOperationError();
  if (retry) void retry();
}
const transfers = ref([]);
const deleting = ref(false);
// 列表不完整只提示一次：常驻横幅会一直占据内容区，顶部消息自动消失即可。
watch(incompleteListing, (value, previous) => { if (value && !previous) app.setInfo?.("cloudBrowser.incomplete"); });

const uploadNotices = new Set();
const {
  downloadDialogOpen, downloadState, downloadQueue, downloadPage, downloadRunning, downloadCanceling,
  downloadResolving, downloadApplyRemaining, downloadPageLoading, downloadSummary, selectDownload, pollDownload,
  loadDownloadPage, cancelDownloadQueue, resolveDownloadConflict, closeDownloadQueue, revealDownloadRoot, disposeDownload,
} = useDownloadQueue({
  captureScope: () => ({ connectionId: props.connectionId, bucket: activeBucket.value }),
  scopeValid: (scope) => !disposed && scope.connectionId === props.connectionId && scope.bucket === activeBucket.value,
  backend: ({ connectionId }) => ({ backend: "s3", connectionId }),
  onFailure: (cause) => app.handleFailure(cause),
});

// PeerBrowser-style selection: a click selects, Ctrl/Cmd/Alt toggles, Shift
// selects a range and dragging over the listing draws a marquee. A double click
// opens a folder or previews a file; download and delete live in the bottom bar.
const selectedEntry = ref(null);
const selectedEntries = ref([]);
const selectionAnchor = ref(null);
const marquee = ref(null);
let marqueeStart = null;
let suppressClick = false;

function isSelected(item) { return selectedEntries.value.some((entry) => entry.key === item?.key); }
function setSelection(list) {
  selectedEntries.value = list.filter(Boolean);
  selectedEntry.value = selectedEntries.value.length ? selectedEntries.value[selectedEntries.value.length - 1] : null;
}
function clearSelection() {
  selectionAnchor.value = null;
  setSelection([]);
}
const selectionFileCount = computed(() => selectedEntries.value.filter((entry) => !entry.isDir).length);
const selectionDirCount = computed(() => selectedEntries.value.length - selectionFileCount.value);
const selectionTitle = computed(() => selectedEntries.value.length > 1
  ? t("peer.selectedCount", { count: selectedEntries.value.length })
  : selectedEntry.value?.name ?? "");
const selectionSubtitle = computed(() => {
  if (selectedEntries.value.length > 1) {
    return t("peer.selectedSummary", { files: selectionFileCount.value, dirs: selectionDirCount.value });
  }
  if (!selectedEntry.value) return "";
  return entryKind(selectedEntry.value) + (selectedEntry.value.isDir ? "" : ` · ${formatBytes(selectedEntry.value.size)}`);
});
const selectionGlyph = computed(() => (selectedEntries.value.length > 1
  ? { isDir: true, name: "" }
  : selectedEntry.value));
function selectSingle(item) {
  selectionAnchor.value = item.key;
  setSelection([item]);
  if (viewMode.value === "gallery" && !item.isDir && canPreview(item)
    && (previewKey.value !== item.key || (!previewData.value && !previewLimit.value))) {
    void previewEntry(item);
  }
}
// 选择栏与 LAN 浏览器一致：选中目录时提供显式「打开」，多选时可就地清除选择。
function runSelectionAction(action) {
  const entries = selectedEntries.value.slice();
  clearSelection();
  if (entries.length) return action(entries);
}
function openSelection(entries) {
  if (entries[0]?.isDir) navigate(entries[0].key);
}
// WebKitGTK can drop modifier bits from click after mousedown is handled, so the
// pointer and held-key state are trusted alongside the click event.
const pointerModifiers = { ctrl: false, meta: false, alt: false, shift: false };
const heldModifiers = { ctrl: false, meta: false, alt: false, shift: false };
function markModifierFlag(target, event, active) {
  if (event.key === "Control" || event.code === "ControlLeft" || event.code === "ControlRight") target.ctrl = active;
  else if (event.key === "Meta" || event.code === "MetaLeft" || event.code === "MetaRight") target.meta = active;
  else if (event.key === "Alt" || event.code === "AltLeft" || event.code === "AltRight") target.alt = active;
  else if (event.key === "Shift" || event.code === "ShiftLeft" || event.code === "ShiftRight") target.shift = active;
}
function onModifierKeydown(event) { markModifierFlag(heldModifiers, event, true); }
function onModifierKeyup(event) { markModifierFlag(heldModifiers, event, false); }
function resetHeldModifiers() {
  heldModifiers.ctrl = false; heldModifiers.meta = false; heldModifiers.alt = false; heldModifiers.shift = false;
}
function hasSelectionModifier(event) {
  return Boolean(
    event?.ctrlKey || event?.metaKey || event?.altKey
    || pointerModifiers.ctrl || pointerModifiers.meta || pointerModifiers.alt
    || heldModifiers.ctrl || heldModifiers.meta || heldModifiers.alt,
  );
}
function selectEntry(item, event) {
  if (!item || suppressClick) return;
  const additive = hasSelectionModifier(event);
  const ranged = Boolean(event?.shiftKey || pointerModifiers.shift || heldModifiers.shift);
  pointerModifiers.ctrl = false; pointerModifiers.meta = false;
  pointerModifiers.alt = false; pointerModifiers.shift = false;
  const result = selectionForClick({ entries: entries.value, selection: selectedEntries.value,
    anchor: selectionAnchor.value, entry: item, identityField: "key", additive, ranged });
  if (result) {
    setSelection(result.selection);
    selectionAnchor.value = result.anchor;
    return;
  }
  selectSingle(item);
}
// Dragging anywhere over the listing draws a selection band; a plain click on
// empty space clears the selection.
const fileViewEl = ref(null);
function onFileViewMousedown(event) {
  pointerModifiers.ctrl = Boolean(event.ctrlKey);
  pointerModifiers.meta = Boolean(event.metaKey);
  pointerModifiers.alt = Boolean(event.altKey);
  pointerModifiers.shift = Boolean(event.shiftKey);
  if (event.button !== 0 || viewMode.value === "columns") return;
  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  const target = event.target;
  if (target?.closest?.("a, input, textarea, select, [contenteditable], pre, video, audio")) return;
  if (!target?.closest?.("[data-key]")
    && target?.closest?.("button, .pathbar, .selection-bar, .cloud-transfers, thead")) return;
  event.preventDefault();
  fileViewEl.value?.focus();
  marqueeStart = { clientX: event.clientX, clientY: event.clientY };
  marquee.value = { left: event.clientX, top: event.clientY, width: 0, height: 0 };
  window.addEventListener("mousemove", onMarqueeMove);
  window.addEventListener("mouseup", onMarqueeUp);
}
function onMarqueeMove(event) {
  if (!marqueeStart) return;
  const width = Math.abs(event.clientX - marqueeStart.clientX);
  const height = Math.abs(event.clientY - marqueeStart.clientY);
  if (width <= 3 && height <= 3) return;
  marquee.value = {
    left: Math.min(marqueeStart.clientX, event.clientX),
    top: Math.min(marqueeStart.clientY, event.clientY),
    width,
    height,
  };
  const band = marquee.value;
  const hits = [];
  for (const element of fileViewEl.value?.querySelectorAll("[data-key]") ?? []) {
    const rect = element.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    if (rect.left < band.left + band.width && rect.right > band.left
      && rect.top < band.top + band.height && rect.bottom > band.top) {
      hits.push(element.getAttribute("data-key"));
    }
  }
  setSelection(entries.value.filter((entry) => hits.includes(entry.key)));
}
function onMarqueeUp(event) {
  window.removeEventListener("mousemove", onMarqueeMove);
  window.removeEventListener("mouseup", onMarqueeUp);
  const band = marquee.value;
  const dragged = Boolean(band) && (band.width > 3 || band.height > 3);
  if (dragged) {
    // The click that ends a drag would otherwise collapse the selection.
    suppressClick = true;
    setTimeout(() => { suppressClick = false; }, 0);
  } else if (!event.target?.closest?.("[data-key]")) {
    clearSelection();
  }
  marquee.value = null;
  marqueeStart = null;
}
const marqueeStyle = computed(() => {
  const band = marquee.value;
  const view = fileViewEl.value;
  if (!band || !view) return {};
  const rect = view.getBoundingClientRect();
  return {
    left: `${band.left - rect.left}px`,
    top: `${band.top - rect.top}px`,
    width: `${band.width}px`,
    height: `${band.height}px`,
  };
});
function openEntry(item) {
  if (!item) return;
  if (item.isDir) { navigate(item.key); return; }
  if (!isSelected(item)) setSelection([item]);
  if (canPreview(item)) void previewEntry(item);
}

// 画廊用左右方向键、其余视图用上下方向键在可见条目间移动选中项，并循环切换：
// 首个按上/左跳到末尾，末尾按下/右跳到首个。
function navigateSelection(delta) {
  const listing = entries.value;
  const length = listing.length;
  if (!length) return;
  const current = listing.findIndex((entry) => entry.key === selectedEntry.value?.key);
  const index = current === -1
    ? (delta > 0 ? 0 : length - 1)
    : (current + delta + length) % length;
  selectSingle(listing[index]);
  void nextTick(() => {
    fileViewEl.value?.querySelector(".is-selected")?.focus?.();
  });
}

// 返回上一级：Alt+↑（Windows 资源管理器、GNOME Files）与 macOS 的 ⌘+↑ 都支持；
// 存储桶根目录不做任何操作。
function goUpLevel() {
  const crumbs = breadcrumbs.value;
  if (crumbs.length <= 1) return;
  navigate(crumbs[crumbs.length - 2].path);
}

function onFileViewKeydown(event) {
  const target = event.target;
  if (target?.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName)) return;
  if (event.key === "ArrowUp" && (event.altKey || event.metaKey)) {
    event.preventDefault();
    goUpLevel();
    return;
  }
  const gallery = viewMode.value === "gallery";
  const forward = gallery ? "ArrowRight" : "ArrowDown";
  const back = gallery ? "ArrowLeft" : "ArrowUp";
  if (event.key !== forward && event.key !== back) return;
  event.preventDefault();
  navigateSelection(event.key === forward ? 1 : -1);
}

// 快捷键说明面板：只列本视图真实实现的行为，文案与 LAN 浏览器共用。
// 面板 Teleport 到 body 并自行定位，点击外部或按 Esc 立即收起。
const shortcutsOpen = ref(false);
const shortcutsTriggerEl = ref(null);
const shortcutsPanelEl = ref(null);
const shortcutsPanelStyle = ref({});
const isMacPlatform = /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent || "");
const shortcutRows = computed(() => {
  const selectMods = isMacPlatform ? "⌘ / Ctrl / Alt" : "Ctrl / Alt";
  const upMods = isMacPlatform ? "Alt / ⌘" : "Alt";
  return [
    { id: "add", keys: t("peer.shortcutAddKeys", { mods: selectMods }), desc: t("peer.shortcutAddDesc") },
    { id: "range", keys: t("peer.shortcutRangeKeys"), desc: t("peer.shortcutRangeDesc") },
    { id: "marquee", keys: t("peer.shortcutMarqueeKeys"), desc: t("peer.shortcutMarqueeDesc") },
    { id: "open", keys: t("peer.shortcutOpenKeys"), desc: t("peer.shortcutOpenDesc") },
    { id: "enter", keys: t("peer.shortcutEnterKeys"), desc: t("peer.shortcutEnterDesc") },
    { id: "step", keys: t("peer.shortcutStepKeys"), desc: t("peer.shortcutStepDesc") },
    { id: "up", keys: t("peer.shortcutUpKeys", { mods: upMods }), desc: t("peer.shortcutUpDesc") },
    { id: "esc", keys: t("peer.shortcutEscKeys"), desc: t("peer.shortcutEscDesc") },
  ];
});
// 面板贴在按钮正下方、右对齐；jsdom 等拿不到尺寸的环境跳过定位，不影响内容渲染。
function positionShortcutsPanel() {
  const rect = shortcutsTriggerEl.value?.getBoundingClientRect?.();
  if (!rect || !rect.width) return;
  shortcutsPanelStyle.value = {
    position: "fixed",
    top: `${Math.round(rect.bottom + 6)}px`,
    right: `${Math.round(Math.max(8, window.innerWidth - rect.right))}px`,
  };
}
function closeShortcutsPanel() {
  shortcutsOpen.value = false;
}
function toggleShortcutsPanel() {
  if (shortcutsOpen.value) { closeShortcutsPanel(); return; }
  shortcutsOpen.value = true;
  void nextTick(positionShortcutsPanel);
}
function onShortcutsOutsideMousedown(event) {
  if (!shortcutsOpen.value) return;
  const target = event.target;
  if (shortcutsPanelEl.value?.contains?.(target) || shortcutsTriggerEl.value?.contains?.(target)) return;
  closeShortcutsPanel();
}
function onShortcutsKeydown(event) {
  if (event.key === "Escape" && shortcutsOpen.value) closeShortcutsPanel();
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
  // Previewing one member of a multi selection must not collapse the selection.
  if (isSelected(entry)) selectedEntry.value = entry;
  else setSelection([entry]);
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
    crumbs.push({ name: directoryName(segment), path });
  }
  return crumbs;
});
const breadcrumbsEl = ref(null);
watch(
  breadcrumbs,
  () => {
    const element = breadcrumbsEl.value;
    if (element) element.scrollLeft = element.scrollWidth;
  },
  { flush: "post" },
);
// 路径一深，靠右的面包屑会被裁剪；滚轮在路径栏上转换为横向滚动。
function scrollBreadcrumbs(event) {
  const element = event.currentTarget;
  if (!element || element.scrollWidth <= element.clientWidth) return;
  if (Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
  event.preventDefault();
  element.scrollLeft += event.deltaY;
}

// 新列或 loading 占位列出现时滚到最右，保证反馈落在视野内而不是屏幕外。
watch(
  [() => prefix.value, () => entries.value.length, () => columnLoading.value],
  () => {
    void nextTick(() => {
      const element = columnViewEl.value;
      if (element) element.scrollLeft = element.scrollWidth;
    });
  },
  { flush: "post" },
);

// 进入目录后自动聚焦文件区域，方向键无需先点击空白处即可使用；
// 焦点已在区域内或弹窗/输入框上时不抢焦点。
watch(entries, () => {
  void nextTick(() => {
    const view = fileViewEl.value;
    if (!view) return;
    const active = document.activeElement;
    if (active && view.contains(active)) return;
    if (active?.closest?.(".el-overlay, .el-dialog, .el-message-box, .preview-dialog, .edit-panel")) return;
    if (active?.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(active?.tagName)) return;
    view.focus();
  });
});
const isEncryptedDrivePath = (path) => path.startsWith('_peerdav_encrypt/');
const insideEncryptedDrive = computed(() => isEncryptedDrivePath(prefix.value));
const hasProtectedObjects = computed(() => objects.value.some((item) => item.protected === true));
const targetProtected = (target) => protectedRoot.value || objects.value.some((item) => item.protected === true && item.key.startsWith(target));
const canMutate = computed(() => prepared.value && !preparing.value && !protectedRoot.value && !busy.value && !loading.value && !uploadRunning.value);
const current = () => ({ id: requestId, connectionId: props.connectionId, prefix: prefix.value });
const valid = (scope) => !disposed && scope.id === requestId && scope.connectionId === props.connectionId && scope.prefix === prefix.value;
function handleAuthenticationFailure(cause) {
  if (["interface-lock", "unlock", "initialize", "change-admin"].includes(errorStrategy(cause))) app.handleFailure(cause);
}
function fail(cause, scope, retry = null) {
  if (!valid(scope) || errorStrategy(cause) === "ignore") return;
  handleAuthenticationFailure(cause);
  retryOperation.value = retry;
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
  } catch (cause) { fail(cause, scope, () => valid(scope) && load(more)); }
  finally { if (valid(scope)) loading.value = false; }
}
function navigate(value) {
  if (!value.startsWith(root.value)) return;
  requestId++; prefix.value = value; objects.value = []; next.value = null; complete.value = false;
  protectedRoot.value = false; busy.value = false;
  clearSelection();
  columnVersion++; columnAncestors.value = [];
  clearPreview();
  void load();
  if (viewMode.value === "columns") void rebuildColumns();
}
// Refresh keeps the current listing on screen until the new page arrives, so a
// transient network failure never looks like the folder became empty.
function refresh() {
  requestId++;
  next.value = null; complete.value = false; busy.value = false;
  clearSelection(); clearPreview();
  void load();
  if (viewMode.value === "columns") void rebuildColumns();
}
async function operate(action) {
  if (busy.value) return;
  const scope = current(); busy.value = true; error.value = null;
  try { await action(scope); } catch (cause) { fail(cause, scope, () => valid(scope) && operate(action)); }
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
    if (isCurrent()) { handleAuthenticationFailure(cause); preparationError.value = normalizeAppError(cause); if (cause?.code === "CLOUD_OBJECT_PROTECTED") protectedRoot.value = true; }
  } finally { if (isCurrent()) preparing.value = false; }
}
function remember(transfer, scope) {
  if (!valid(scope)) return;
  statusVersion++;
  transfers.value = [...transfers.value.filter((item) => item.transferId !== transfer.transferId), transfer];
  if (transfer.error && transfer.phase !== "cancelled") fail(transfer.error, scope);
}
function upload() { return uploadDirectory("single"); }
// Cloud and LAN share the bounded-page session, but retain backend safety policies.
const {
  uploadDialogOpen, uploadSummary, uploadPage, uploadRunning,
  uploadBatchError, uploadPageNotice, uploadApplyRemaining, uploadResolving, uploadOverwriteAllowed,
  selectUpload: uploadDirectory, pollUpload, cancelUpload, closeUpload,
  resolveUploadConflict, cancelUploadSession,
} = useUploadBatch({
  adapter: cloudUploadAdapter,
  captureScope: () => ({ connectionId: props.connectionId, bucket: activeBucket.value, prefix: prefix.value }),
  scopeValid: (scope) => !disposed && scope.connectionId === props.connectionId && scope.bucket === activeBucket.value,
  canSelect: () => canMutate.value,
  onFailure: (cause) => app.handleFailure(cause),
  onCompleted: (summary, scope) => {
    app.setNotice?.("cloudBrowser.directoryUploaded", { count: summary.completedFiles, bytes: formatBytes(summary.completedBytes) });
    if (prefix.value === scope.prefix) refresh();
  },
});
function download(item) {
  if (!item || item.isDir) return;
  return selectDownload({ mode: "single", path: item.key });
}
function downloadSelection(entries) {
  if (!entries.length) return;
  if (entries.length === 1) return selectDownload({ mode: entries[0].isDir ? "folder" : "single", path: entries[0].key });
  return selectDownload({ mode: "files", paths: entries.map((entry) => entry.key) });
}
function validDeletePlan(plan, target) {
  return plan != null && Number.isSafeInteger(plan.objectCount) && plan.objectCount >= 0 && Array.isArray(plan.entries)
    && plan.entries.length === plan.objectCount && new Set(plan.entries.map((item) => item?.key)).size === plan.objectCount
    && plan.entries.every((item) => typeof item?.key === "string" && item.key.startsWith(target)
      && !/[\\\0:]/.test(item.key) && item.key.split("/").every((part) => part && part !== "." && part !== "..")
      && Number.isSafeInteger(item.size) && item.size >= 0 && (item.etag === null || typeof item.etag === "string"))
    && typeof plan.planId === "string" && plan.planId.length > 0 && Number.isFinite(plan.expiresAtUnix);
}
// 删除确认：Element Plus 的标题只接受纯文本，故把红色感叹号图标 + 标题渲染在消息体顶部，
// 由 confirmAction 的 headerless 模式隐藏原生标题、把关闭按钮浮到右上角（样式见 src/assets/confirm.css）。
function deleteDialogTitle() {
  return h("div", { class: "confirm-heading" }, [
    h("span", { class: "confirm-heading__icon" }, [
      h("svg", { viewBox: "0 0 16 16", width: 20, height: 20, "aria-hidden": "true", focusable: "false", style: { display: "block" } }, [
        h("circle", { cx: 8, cy: 8, r: 7, fill: "currentColor" }),
        h("path", { d: "M8 4.4v4.2M8 11.2v.01", stroke: "#fff", "stroke-width": 1.7, "stroke-linecap": "round" }),
      ]),
    ]),
    h("span", t("cloudBrowser.deleteTitle")),
  ]);
}
// S3 键可含换行等控制字符：直接渲染会让一个对象在清单里看起来像多行，
// 因此仅在存在歧义字符时显示 JSON 转义形式，普通键保持原样可读。
function displayKey(value) {
  return /[\n\r\t"]/.test(value) || value !== value.trim() ? JSON.stringify(value) : value;
}
function deleteDialogBody(plan, target) {
  return h("div", [
    deleteDialogTitle(),
    h("p", { class: "confirm-summary" }, t("cloudBrowser.deleteSummary", { count: plan.objectCount })),
    h("p", { class: "confirm-summary" }, t("cloudBrowser.deleteConcurrency")),
    h("div", { class: "confirm-scope" }, [
      h("span", { class: "confirm-scope__label" }, t("cloudBrowser.deleteScopeLabel")),
      h("span", { class: "confirm-scope__value", "data-test": "delete-target" }, target ? displayKey(target) : t("cloudBrowser.deleteRootPrefix")),
    ]),
    h("ul", { class: "confirm-list", "data-test": "delete-keys" }, plan.entries.map((item) => h("li", { key: item.key }, displayKey(item.key)))),
  ]);
}
async function remove(target) {
  // 类似禁止 `rm -rf /`：未指定目录（根前缀）时拒绝整体删除，根目录下的对象必须逐个删除。
  if (!target || target === "/") {
    error.value = { code: "CLOUD_DELETE_ROOT_FORBIDDEN", message: t("cloudBrowser.deleteRootForbidden") };
    return;
  }
  if (!canMutate.value || targetProtected(target) || deleting.value) return;
  // 删除进行中：按钮显示 loading 并禁用；确认框在用户确认后立即关闭，不再叠加任何常驻提示。
  deleting.value = true;
  try { await operate(async (scope) => {
    const plan = await cloudObjects.deletePlan({ connectionId: scope.connectionId, prefix: target });
    if (!valid(scope)) return;
    if (!validDeletePlan(plan, target)) { error.value = { code: "CLOUD_CONFIRMATION_REQUIRED", message: t("cloudBrowser.invalidPlan") }; return; }
    const accepted = await confirmAction(deleteDialogBody(plan, target), "", { headerless: true, danger: true, type: null, confirmText: t("common.delete") });
    if (!accepted || !valid(scope)) return;
    if (Date.now() >= plan.expiresAtUnix * 1000) { error.value = { code: "CLOUD_DELETE_PLAN_STALE", message: t("cloudBrowser.stalePlan") }; return; }
    const result = await cloudObjects.deleteExecute({ connectionId: scope.connectionId, planId: plan.planId, confirmed: true });
    if (!valid(scope)) return;
    // 结果只走顶部自动消失的消息，不留常驻提示条；失败原因取最近的一条直接展示。
    if (result.failed.length) {
      app.setError?.("cloudBrowser.deletePartial", { deleted: result.deleted.length, failed: result.failed.length, reason: result.failed[0].error?.message || t("cloudBrowser.deleteFailedUnknown") });
    } else if (result.deleted.length) {
      app.setNotice?.("cloudBrowser.deletedObjects", { count: result.deleted.length });
    }
    for (const item of result.failed) if (item.error?.code === "CLOUD_OBJECT_PROTECTED") fail(item.error, scope);
    await load();
  }); } finally { deleting.value = false; }
}
async function acknowledge(item) {
  if (item.phase !== "unknown" || busy.value) return;
  await operate(async (scope) => {
    const accepted = await confirmAction(h("div", [
      h("p", { class: "confirm-summary" }, t("cloudBrowser.acknowledgeBody")),
      h("div", { class: "confirm-scope" }, [
        h("span", { class: "confirm-scope__label" }, t("cloudBrowser.acknowledgeTargetLabel")),
        h("span", { class: "confirm-scope__value" }, item.key == null ? t("cloudBrowser.unknownTarget") : displayKey(item.key)),
      ]),
    ]), t("cloudBrowser.acknowledge"), { confirmText: t("cloudBrowser.acknowledgeAction") });
    if (!accepted || !valid(scope)) return;
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
      const scoped = result.filter((item) => item.connectionId === scope.connectionId);
      let refreshAfterUpload = false;
      for (const item of scoped) {
        if (item.error?.code === "CLOUD_OBJECT_PROTECTED") {
          protectedRoot.value = true;
          fail(item.error, scope);
        }
        // A completed upload becomes one toast and leaves the list, so the
        // notification pane never grows with historical successes.
        if (item.direction === "upload" && item.phase === "completed" && !uploadNotices.has(item.transferId)) {
          uploadNotices.add(item.transferId);
          app.setNotice?.("cloudBrowser.uploaded", { key: item.key ?? "" });
          refreshAfterUpload = true;
        }
      }
      transfers.value = scoped.filter((item) => !["completed", "cancelled", "acknowledged"].includes(item.phase));
      if (refreshAfterUpload) refresh();
    }
  } catch (cause) {
    if (version === statusVersion) fail(cause, scope);
  }
  finally { polling = false; }
}
// The catalog decides which bucket exists, but the backend session must be
// explicitly rebound to the first bucket before any listing is requested.
async function bootstrap() {
  await refreshCatalog();
  if (disposed) return;
  // 刷新后停留在同步盘管理视图时，不强制绑定存储桶回到文件首页。
  if (view.value === "management") return;
  if (view.value === "local" && localDrive.value) { await loadLocal(); return; }
  const bucket = buckets.value[0];
  if (bucket) await selectBucket(bucket);
  else { void prepare(); void load(); }
}
watch(() => props.connectionId, (_id, prevId) => {
  void disposeDownload();
  void cancelUploadSession();
  resetLocalScope();
  requestId++; statusVersion++; preparationVersion++; uploadNotices.clear(); prepared.value = false; preparing.value = false; preparationError.value = null; protectedRoot.value = false; connections.value = [];
  // 首次 immediate 触发保留持久化恢复的视图与聚焦盘；仅在连接真正切换时才回到文件首页。
  if (prevId !== undefined) { view.value = "files"; activeDriveId.value = ""; }
  catalogDrives.value = []; buckets.value = []; activeBucket.value = "";
  objects.value = []; transfers.value = []; prefix.value = ""; root.value = ""; next.value = null; complete.value = false; busy.value = false; loading.value = false; error.value = null;
  clearSelection();
  columnVersion++; columnAncestors.value = [];
  clearPreview();
  void bootstrap();
}, { immediate: true });
onMounted(() => {
  void poll();
  timer = setInterval(() => { void pollUpload(); void pollDownload(); void poll(); void pollLocalSync(); }, 1500);
  titlebarActions.value = Boolean(typeof document !== "undefined" && document.getElementById("titlebar-file-actions"));
  window.addEventListener("keydown", onModifierKeydown, true);
  window.addEventListener("keyup", onModifierKeyup, true);
  window.addEventListener("blur", resetHeldModifiers);
  window.addEventListener("mousedown", onShortcutsOutsideMousedown, true);
  window.addEventListener("keydown", onShortcutsKeydown, true);
});
onBeforeUnmount(() => {
  void disposeDownload();
  void cancelUploadSession();
  disposed = true; localVersion++; requestId++; statusVersion++; clearInterval(timer);
  clearTimeout(loadingHintTimer); clearTimeout(checkingHintTimer);
  window.removeEventListener("keydown", onModifierKeydown, true);
  window.removeEventListener("keyup", onModifierKeyup, true);
  window.removeEventListener("blur", resetHeldModifiers);
  window.removeEventListener("mousedown", onShortcutsOutsideMousedown, true);
  window.removeEventListener("keydown", onShortcutsKeydown, true);
  window.removeEventListener("mousemove", onMarqueeMove);
  window.removeEventListener("mouseup", onMarqueeUp);
});
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
          <button type="button" class="sidebar-section-manage" :class="{ 'is-active': view === 'management' && !activeDriveId }" @click="openManagement()">{{ t('drive.manageActions') }}</button>
        </div>
        <button v-for="(drive, index) in drives" :key="index" type="button" class="share-link drive-link" :class="{ 'is-active': activeDriveId === drive.id }" @click="openLocal(drive)">
          <FileGlyph :entry="{ isDir: true, name: drive.label, driveKind: drive.driveKind || 'encrypted-drive' }" /><span>{{ drive.label }}</span>
        </button>
        <p v-if="!drives.length" class="sidebar-empty">{{ t('cloudBrowser.noSyncDrives') }}</p>
      </aside>
      <main class="browser-main">
        <div v-if="view === 'management'" class="management-view">
          <div class="management-content"><CloudSyncDrives :key="connectionId" :connection-id="connectionId" :bucket="activeBucket" :focus-id="localDrive?.repositoryId || activeDriveId" @changed="refreshCatalog" /></div>
        </div>
        <template v-else-if="view === 'local'">
          <div class="pathbar">
            <nav class="breadcrumbs" :aria-label="t('drive.localFolder')"><button v-for="crumb in localCrumbs" :key="crumb.path" type="button" class="breadcrumb" @click="localPath = crumb.path; localSelection = []">{{ crumb.name }} ›</button></nav>
            <el-button :disabled="localLoading || localBusy" @click="loadLocal">{{ t('cloudBrowser.refresh') }}</el-button>
            <el-button @click="openManagement(activeDriveId)">{{ t('drive.manageActions') }}</el-button>
          </div>
          <div class="local-toolbar">
            <div class="view-switch" role="group" :aria-label="t('peer.viewMode')"><button v-for="mode in VIEW_MODES" :key="mode" type="button" :aria-label="t(`cloudBrowser.${mode}`)" :aria-pressed="viewMode === mode" @click="setViewMode(mode)">{{ t(`cloudBrowser.${mode}`) }}</button></div>
            <el-button type="primary" :loading="localBusy" :disabled="localBusy || localLoading || localRunning" @click="previewLocalSync">{{ t('drive.syncNow') }}</el-button>
            <span v-if="localPhase" role="status">{{ t(`cloud.phase${localPhase[0].toUpperCase()}${localPhase.slice(1)}`) }}</span>
          </div>
          <p v-if="localStatusLoading" role="status" class="hint">{{ t('drive.checkingStatus') }}</p>
          <p class="hint">{{ t('drive.syncWholeHint') }}</p>
          <p v-if="localError" role="alert">{{ localError.message }}</p>
          <div v-if="localPreview" class="local-plan" role="region" :aria-label="t('cloud.preview')">
            <p class="hint">{{ t('drive.checkedAt') }} <time :datetime="localPreview.checkedAt">{{ new Date(localPreview.checkedAt).toLocaleString() }}</time></p>
            <p v-if="!localPreviewHasChanges" role="status">{{ t('drive.upToDate') }}</p>
            <p>{{ t('cloud.previewUploads') }}: {{ localPreview.summary?.uploads || 0 }} · {{ t('cloud.previewDownloads') }}: {{ localPreview.summary?.downloads || 0 }} · {{ t('cloud.previewDeletes') }}: {{ localPreview.summary?.deletes || 0 }} · {{ t('cloud.previewConflicts') }}: {{ localPreview.summary?.conflicts || 0 }}</p>
            <el-button v-if="localPreviewHasChanges" :loading="localBusy" :disabled="localBusy" type="primary" @click="applyLocalSync">{{ t('drive.applyPlan') }}</el-button><el-button :disabled="localBusy" @click="localPreview = null">{{ t('common.cancel') }}</el-button>
          </div>
          <div class="file-view local-file-view">
            <FileManagerView :entries="localEntries" :mode="viewMode" :selection="localSelection" :columns="localColumns" :loading="localLoading" :error="localError"
              :entry-kind="entryKind" :format-size="formatSize" :entry-status="entry => !localStatusKnown || entry.synced ? '' : t('drive.pendingSync')"
              @select="selectLocal" @activate="activateLocal" @column-select="activateLocal" @preview="activateLocal" @status-action="previewLocalSync">
              <template #gallery-preview><div v-if="localSelection[0]" class="gallery-placeholder"><FileGlyph :entry="localSelection[0]" size="xl" /><strong>{{ localSelection[0].name }}</strong><span>{{ formatSize(localSelection[0]) }}</span></div><p v-else>{{ t('peer.selectPreview') }}</p></template>
            </FileManagerView>
          </div>
        </template>
        <template v-else>
          <div class="pathbar">
            <div ref="breadcrumbsEl" class="breadcrumbs" aria-label="Breadcrumb" @wheel="scrollBreadcrumbs">
              <template v-for="(crumb, index) in breadcrumbs" :key="crumb.path">
                <span v-if="index" class="breadcrumb-sep" aria-hidden="true">›</span>
                <button type="button" class="breadcrumb" :class="{ 'is-current': index === breadcrumbs.length - 1 }" @click="navigate(crumb.path)">{{ crumb.name }}</button>
              </template>
            </div>
            <BrowserSkeleton v-if="checkingHint" class="pathbar-status" compact />
          </div>
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
                <el-tooltip :content="t('peer.uploadFolder')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" data-test="upload-directory" :aria-label="t('peer.uploadFolder')" :disabled="!canMutate" @click="uploadDirectory('folder')">
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" /><path d="M12 17v-5M9.5 14.5 12 12l2.5 2.5" /></svg>
                  </button>
                </el-tooltip>
                <el-tooltip v-if="!insideEncryptedDrive" :content="prefix ? t('cloudBrowser.deletePrefix') : t('cloudBrowser.deletePrefixRootHint')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                  <button type="button" class="view-switch__button" data-test="delete-prefix" :aria-label="t('cloudBrowser.deletePrefix')" :disabled="deleting || !canMutate || hasProtectedObjects" :aria-busy="deleting ? 'true' : undefined" @click="remove(prefix)">
                    <svg v-if="deleting" class="action-spinner" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-dasharray="34 20" /></svg>
                    <svg v-else viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16" /><path d="M9 7V5h6v2" /><path d="M6 7l1 12h10l1-12" /></svg>
                  </button>
                </el-tooltip>
                <button ref="shortcutsTriggerEl" type="button" class="view-switch__button shortcuts-trigger" :aria-label="t('peer.shortcuts')" :aria-expanded="shortcutsOpen ? 'true' : 'false'" @click="toggleShortcutsPanel">
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="2.5" y="6.5" width="19" height="11" rx="2" />
                    <path d="M6 10h1.5M9.5 10h1.5M13 10h1.5M16.5 10h1.5" />
                    <path d="M6 13h1.5M9.5 13h1.5M13 13h1.5M16.5 13h1.5" />
                    <path d="M8.5 15.5h7" />
                  </svg>
                </button>
              </div>
            </div>
          </Teleport>
          <Teleport to="body">
            <div v-if="shortcutsOpen" ref="shortcutsPanelEl" class="shortcuts-panel" :style="shortcutsPanelStyle" role="dialog" :aria-label="t('peer.shortcutsTitle')">
              <h4 class="shortcuts-panel__title">{{ t("peer.shortcutsTitle") }}</h4>
              <dl class="shortcuts-panel__list">
                <div v-for="row in shortcutRows" :key="row.id" class="shortcuts-panel__row">
                  <dt class="shortcuts-panel__keys"><kbd>{{ row.keys }}</kbd></dt>
                  <dd class="shortcuts-panel__desc">{{ row.desc }}</dd>
                </div>
              </dl>
              <p class="shortcuts-panel__note">{{ t("peer.shortcutsFootnote") }}</p>
            </div>
          </Teleport>
          <OperationErrorDialog :error="visibleError" :retryable="!!preparationError || !!retryOperation" :recheck="!!preparationError" :busy="busy || loading || preparing" @close="closeOperationError" @retry="retryFailedOperation" />
          <BrowserSkeleton v-if="loadingHint && entries.length" compact />

          <div ref="fileViewEl" class="file-view" tabindex="0" @keydown="onFileViewKeydown" @mousedown="onFileViewMousedown">
            <div v-if="marquee" class="marquee" :style="marqueeStyle" aria-hidden="true"></div>
            <FileManagerView :entries="entries" :mode="viewMode" :selection="selectedEntries" identity-field="key"
              :columns="columns" column-identity-field="prefix" :column-loading="columnLoading"
              :loading="showLoading" :complete="complete" :error="visibleError" :partial="incompleteListing"
              :empty-label="t('cloudBrowser.empty')" :time-column="timeColumn || ''"
              :time-label="t(timeColumn === 'modified' ? 'peer.modified' : 'cloudBrowser.created')"
              :entry-kind="entryKind" :format-size="formatSize" :format-modified="formatModified"
              @select="selectEntry" @activate="openEntry" @column-select="openColumn" @preview="previewEntry"
              @column-element="columnViewEl = $event" @column-wheel="scrollColumnView">
              <template #gallery-preview>
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
                <FileGlyph :entry="{ isDir: true, name: '' }" size="xl" />
                <strong>{{ t('peer.selectPreview') }}</strong>
                <span>{{ t('peer.selectPreviewHint') }}</span>
              </div>
              </template>
              <template #pagination>
                <el-button v-if="next && !showLoading" data-test="more" class="load-more" :disabled="loading" @click="load(true)">{{ t('cloudBrowser.more') }}</el-button>
              </template>
            </FileManagerView>
          </div>
          <div v-if="selectedEntries.length" class="selection-bar" :class="{ 'selection-bar--reserved': !selectedEntries.length, 'selection-bar--multi': selectedEntries.length > 1 }">
            <div v-if="selectedEntries.length" class="selection-summary">
              <FileGlyph :entry="selectionGlyph" />
              <div>
                <strong>{{ selectionTitle }}</strong>
                <span>{{ selectionSubtitle }}<template v-if="selectedEntry?.protected"> · {{ t('cloudBrowser.protected') }}</template></span>
              </div>
              <el-button v-if="selectedEntries.length > 1" class="selection-clear" link type="primary" :disabled="busy" @click="clearSelection">{{ t('peer.clearSelection') }}</el-button>
            </div>
            <p v-else class="selection-bar__hint">{{ t('peer.selectionHint') }}</p>
            <div v-if="selectedEntries.length" class="selection-actions">
              <el-button v-if="viewMode !== 'columns' && selectedEntries.length === 1 && selectedEntry.isDir" type="primary" :disabled="busy" @click="runSelectionAction(openSelection)">{{ t('peer.open') }}</el-button>
              <el-button data-test="download" :type="selectedEntries.length > 1 ? 'primary' : undefined" plain :disabled="busy" @click="runSelectionAction(downloadSelection)">{{ t('cloudBrowser.download') }}</el-button>
              <el-button v-if="selectedEntries.length === 1 && !insideEncryptedDrive && !isEncryptedDrivePath(selectedEntry.key)" data-test="delete-file" type="danger" plain :loading="deleting" :disabled="deleting || !canMutate || targetProtected(selectedEntry.key)" @click="runSelectionAction(([entry]) => remove(entry.key))">{{ t('cloudBrowser.delete') }}</el-button>
            </div>
          </div>
          <ul v-if="transfers.length" class="cloud-tree cloud-transfers" :aria-label="t('cloudBrowser.transfers')"><li v-for="item in transfers" :key="item.transferId"><span>{{ item.key == null ? t('cloudBrowser.unknownTarget') : JSON.stringify(item.key) }}<span v-if="item.sizeHint != null"> · {{ t('cloudBrowser.bytes', { size: item.sizeHint }) }}</span> · {{ t(`cloudBrowser.${item.direction}`) }} · {{ t(`cloudBrowser.phase.${item.phase}`) }}<strong v-if="item.recoveryRequired || item.phase === 'unknown'"> · {{ t('cloudBrowser.recovery') }}</strong><span v-if="item.error"> · {{ item.error.message }}</span></span><p v-if="item.phase === 'acknowledged'">{{ t('cloudBrowser.acknowledgedNotice') }}</p><el-button v-if="item.phase === 'unknown'" :disabled="busy" @click="acknowledge(item)">{{ t('cloudBrowser.acknowledge') }}</el-button><el-button v-if="item.phase === 'running'" :disabled="busy" @click="cancel(item)">{{ t('cloudBrowser.cancel') }}</el-button></li></ul>
        </template>
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
    <UploadBatchDialog :open="uploadDialogOpen" :summary="uploadSummary" :page="uploadPage" :running="uploadRunning"
      :batch-error="uploadBatchError" :page-notice="uploadPageNotice"
      v-model:apply-remaining="uploadApplyRemaining" :resolving="uploadResolving" :overwrite-allowed="uploadOverwriteAllowed"
      @close="closeUpload" @cancel="cancelUpload" @resolve="resolveUploadConflict" />
    <TransferQueueDialog :open="downloadDialogOpen" :items="downloadQueue" :running="downloadRunning" :canceling="downloadCanceling"
      :summary="downloadSummary" :queue-state="downloadState" :next-offset="downloadPage.nextOffset" :page-loading="downloadPageLoading"
      v-model:apply-remaining="downloadApplyRemaining" :resolving="downloadResolving" @resolve="resolveDownloadConflict" @page="loadDownloadPage"
      @cancel="cancelDownloadQueue" @close="closeDownloadQueue" @reveal="revealDownloadRoot" />
  </section>
</template>

<style scoped>
.local-toolbar { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding: 8px 0; }
.local-toolbar .view-switch button { padding: 6px 10px; border: 1px solid var(--app-border); background: var(--app-surface); color: var(--app-text); border-radius: 6px; cursor: pointer; }
.local-toolbar .view-switch button[aria-pressed=true] { background: var(--selection-bg); }
.local-plan { padding: 10px; border: 1px solid var(--app-border); border-radius: 8px; }
.local-file-view { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.cloud-browser { position: relative; flex: 1; min-height: 0; box-sizing: border-box; display: flex; flex-direction: column; gap: 12px; color: var(--app-text); }

.sidebar-identity { display: flex; flex-direction: column; gap: 2px; margin: 2px 8px 12px; padding-bottom: 12px; border-bottom: 1px solid var(--app-border); }
.sidebar-identity strong, .sidebar-identity span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sidebar-identity strong { font-size: 13px; color: var(--app-text); }
.sidebar-identity span { font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace; font-size: 11px; color: var(--app-text-muted); }

.browser-body { flex: 1; min-height: 0; display: grid; grid-template-columns: 180px minmax(0, 1fr); overflow: hidden; background: transparent; }
.browser-sidebar { min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 3px; padding: 14px 10px; overflow-y: auto; background: color-mix(in oklab, var(--app-surface) 70%, transparent); border-right: 1px solid var(--app-border); }
.sidebar-label { margin: 0 8px 6px; font-size: 11px; font-weight: 600; color: var(--app-text-muted); }
.sidebar-empty { margin: 6px 8px; font-size: 12px; line-height: 1.5; color: var(--app-text-muted); }
.sidebar-section-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; width: 100%; margin: 10px 8px 6px; }
.sidebar-section-head .sidebar-label { margin: 0; }
.sidebar-section-manage { flex: 0 0 auto; padding: 2px 9px; border: 1px solid transparent; border-radius: 999px; background: transparent; color: var(--app-text-muted); font: inherit; font-size: 11px; cursor: pointer; transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease; }
.sidebar-section-manage:hover { color: var(--app-text); background: var(--app-surface-3); }
.sidebar-section-manage.is-active { color: var(--el-color-primary); border-color: color-mix(in oklab, var(--el-color-primary) 45%, transparent); background: color-mix(in oklab, var(--el-color-primary) 12%, transparent); }
.share-link { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 8px; border: 1px solid transparent; border-radius: 8px; background: transparent; color: var(--app-text-secondary); font: inherit; font-size: 12.5px; text-align: left; cursor: pointer; transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease; }
.share-link :deep(.file-glyph) { width: 18px; height: 18px; }
.share-link span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.share-link:hover { background: var(--app-surface-3); }
.share-link.is-active { color: var(--el-color-primary); border-color: color-mix(in oklab, var(--el-color-primary) 45%, transparent); background: color-mix(in oklab, var(--el-color-primary) 12%, transparent); }

.browser-main { min-width: 0; min-height: 0; overflow: hidden; padding: 14px 16px 18px; display: flex; flex-direction: column; gap: 12px; }
.management-view { position: relative; flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; gap: 12px; overflow: hidden; }
.management-content { flex: 1; min-height: 0; overflow: hidden; }

.pathbar { flex: none; display: flex; align-items: center; gap: 12px; padding-bottom: 12px; border-bottom: 1px solid var(--app-border); }
.breadcrumbs { flex: 1; min-width: 0; display: flex; align-items: center; gap: 2px; overflow-x: auto; overflow-y: hidden; white-space: nowrap; scrollbar-width: none; -ms-overflow-style: none; }
.breadcrumbs::-webkit-scrollbar { display: none; }
.breadcrumb { flex: none; border: 0; border-radius: 6px; padding: 4px 6px; background: transparent; color: var(--app-text-muted); font: inherit; font-size: 12.5px; cursor: pointer; }
.breadcrumb:hover { color: var(--app-text); background: var(--app-surface-2); }
.breadcrumb.is-current { color: var(--app-text); font-weight: 600; }
.breadcrumb-sep { flex: none; color: var(--app-text-muted); font-size: 14px; }
.pathbar-status { flex: none; margin: 0; font-size: 12px; color: var(--app-text-muted); }

.pathbar-actions { flex: none; display: flex; align-items: center; gap: 0; -webkit-app-region: no-drag; }
.titlebar-divider { flex: none; width: 1px; height: 16px; background: var(--app-border); }
.view-switch { flex: none; display: flex; gap: 2px; padding: 0; border: 0; border-radius: 0; background: transparent; }
.view-switch__button { width: 26px; height: 26px; display: inline-flex; align-items: center; justify-content: center; border: 0; border-radius: 999px; background: transparent; color: var(--app-text-muted); cursor: pointer; -webkit-app-region: no-drag; }
.view-switch__button:hover { color: var(--app-text); background: var(--pill-bg); }
.view-switch__button:focus-visible { outline: 2px solid var(--el-color-primary); outline-offset: 1px; }
.view-switch__button:disabled { opacity: 0.4; cursor: default; }
.view-switch__button:disabled:hover { color: var(--app-text-muted); background: transparent; }
.view-switch__button.is-active { color: var(--el-color-primary); background: var(--app-surface); box-shadow: 0 1px 4px rgba(22, 32, 46, 0.1); }
.view-switch__button svg { width: 15px; height: 15px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
/* 快捷键入口是说明类图标，描边略细一点，避免和上传操作抢视觉权重。 */
.shortcuts-trigger svg { stroke-width: 1.4; }

.shortcuts-panel { z-index: 1500; display: grid; gap: 8px; width: 300px; max-height: min(60vh, 420px); overflow: auto; padding: 12px; border: 1px solid var(--app-border); border-radius: 12px; background: var(--app-surface); box-shadow: 0 12px 28px rgba(22, 32, 46, 0.16); color: var(--app-text); }
.shortcuts-panel__title { margin: 0; font-size: 12px; font-weight: 600; color: var(--app-text-muted); }
.shortcuts-panel__list { display: grid; gap: 6px; margin: 0; }
.shortcuts-panel__row { display: grid; grid-template-columns: minmax(104px, auto) 1fr; gap: 10px; align-items: baseline; }
.shortcuts-panel__keys { margin: 0; }
.shortcuts-panel__keys kbd { padding: 2px 6px; border: 1px solid var(--app-border); border-radius: 6px; background: var(--app-surface-2); color: var(--app-text); font-family: inherit; font-size: 11px; white-space: nowrap; }
.shortcuts-panel__desc { margin: 0; font-size: 12px; line-height: 1.5; color: var(--app-text-muted); }
.shortcuts-panel__note { margin: 0; padding-top: 6px; border-top: 1px solid color-mix(in oklab, var(--app-border) 55%, transparent); font-size: 11px; line-height: 1.5; color: var(--app-text-muted); }

.cloud-error { flex: none; display: flex; align-items: center; gap: 10px; padding: 8px 10px; border: 1px solid color-mix(in oklab, var(--el-color-danger) 35%, var(--app-border)); border-radius: 8px; background: color-mix(in oklab, var(--el-color-danger) 7%, transparent); color: var(--el-color-danger); font-size: 12.5px; }
/* 删除进行中的按钮反馈：SVG 变换原点必须显式声明，否则 WebKitGTK 下看不出旋转。 */
.action-spinner { transform-box: view-box; transform-origin: 50% 50%; animation: cloud-spin 0.9s linear infinite; }
@keyframes cloud-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) {
  .action-spinner { animation: none; }
}

.cloud-error__message { min-width: 0; overflow-wrap: anywhere; }

/* Keep the pathbar and selection bar fixed; each view owns its scrolling region. */
.browser-main > .file-view { flex: 1; min-height: 0; overflow: hidden; position: relative; display: flex; flex-direction: column; user-select: none; -webkit-user-select: none; }
/* The region is only a keyboard-nav root; children carry their own focus ring. */
.browser-main > .file-view:focus { outline: none; }
.browser-main > .file-view pre,
.browser-main > .file-view input,
.browser-main > .file-view textarea { user-select: text; -webkit-user-select: text; }

.marquee { position: absolute; z-index: 5; pointer-events: none; border: 1px solid var(--el-color-primary); border-radius: 4px; background: color-mix(in oklab, var(--el-color-primary) 14%, transparent); }
.file-empty { margin: 22px 0; text-align: center; font-size: 13px; color: var(--app-text-muted); }
.file-view--placeholder { flex: 1; min-height: 0; overflow: hidden; }

.load-more { flex: none; align-self: flex-start; }

/* Provider-owned gallery preview content; view layout lives in FileManagerView. */
.gallery-image { max-width: 100%; max-height: 100%; width: auto; height: auto; object-fit: contain; border-radius: 8px; box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16); }
.gallery-audio { width: min(440px, 100%); }
.gallery-video { max-width: 100%; max-height: 100%; width: auto; height: auto; object-fit: contain; border-radius: 8px; background: #000; box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16); }
.gallery-text { margin: 0; box-sizing: border-box; width: 100%; height: 100%; min-height: 0; max-height: none; overflow: auto; padding: 16px; border-radius: 8px; background: var(--app-surface); border: 1px solid var(--app-border); font-size: 12px; line-height: 1.55; white-space: pre-wrap; word-break: break-word; }
.gallery-placeholder { display: flex; flex-direction: column; align-items: center; gap: 7px; color: var(--app-text-muted); text-align: center; }
.gallery-placeholder strong { color: var(--app-text); font-size: 14px; }
.gallery-placeholder span { font-size: 12px; }

.selection-bar { flex: none; box-sizing: border-box; min-height: 54px; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 10px; border: 1px solid var(--app-border); border-radius: 10px; background: var(--app-surface-2); }
.selection-bar--reserved { border-style: dashed; background: transparent; }
.selection-bar--multi { align-items: center; }
.selection-clear { flex: none; margin-left: auto; padding: 0; height: auto; min-height: 0; }
.selection-bar__hint { margin: 0; font-size: 12px; color: var(--app-text-muted); }
.selection-summary { min-width: 0; display: flex; align-items: center; gap: 8px; }
.selection-summary div { min-width: 0; display: flex; flex-direction: column; }
.selection-summary strong, .selection-summary span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.selection-summary strong { font-size: 12.5px; color: var(--app-text); }
.selection-summary span { font-size: 11px; color: var(--app-text-muted); }
.selection-actions { flex: none; display: flex; gap: 6px; }

/* 传输队列沿用 LAN 浏览器的卡片样式，避免横向铺满整个文件区。 */
.cloud-transfers { flex: none; max-height: min(240px, 30vh); overflow: auto; padding: 0; border: 1px solid var(--app-border); border-radius: 10px; background: color-mix(in oklab, var(--app-surface-2) 72%, transparent); }
.cloud-transfers li { padding: 8px 10px; font-size: 12.5px; }

@media (max-width: 760px) {
  .browser-body { grid-template-columns: 1fr; }
  .browser-sidebar { flex-direction: row; align-items: center; overflow-x: auto; border-right: 0; border-bottom: 1px solid var(--app-border); }
  .sidebar-identity { flex: none; margin: 0 10px 0 4px; padding: 0 12px 0 0; border-bottom: 0; border-right: 1px solid var(--app-border); }
  .sidebar-section-head { margin: 0; }
  .sidebar-label, .sidebar-empty { display: none; }
  .share-link { width: auto; flex: none; }
  .selection-bar { align-items: stretch; flex-direction: column; }
  .selection-actions { flex-wrap: wrap; }
  .pathbar { align-items: stretch; flex-direction: column; }
}
.management-view p, .management-view h2, .management-view h3 { margin: 0; }
.management-view p { color: var(--app-text-muted); line-height: 1.6; }
.cloud-tree { margin: 0; padding: 0; list-style: none; }
.cloud-tree li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px; border-bottom: 1px solid var(--app-border); }
.cloud-tree li.is-selected { background: var(--app-surface-3); }
.cloud-browser [role=alert], .cloud-browser li { min-width: 0; overflow-wrap: anywhere; }
.cloud-browser :deep(.el-button + .el-button) { margin-left: 0; }
.cloud-browser [role=alert] { color: var(--el-color-danger); }
@media (max-width: 480px) { .cloud-tree li { flex-wrap: wrap; align-items: flex-start; } }
</style>