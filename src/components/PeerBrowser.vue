<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ElMessageBox } from 'element-plus';
import UploadBatchDialog from "./UploadBatchDialog.vue";
import TransferQueueDialog from "./TransferQueueDialog.vue";
import { useUploadBatch } from "../composables/useUploadBatch";
import { fileUploadAdapter } from "../lib/uploadBatchAdapters";
import { useDownloadQueue } from "../composables/useDownloadQueue";

import DirectoryTreePicker from './DirectoryTreePicker.vue';
import FileGlyph from "./FileGlyph.vue";
import { selectionForClick } from "../lib/fileManagerAdapters";
import FileManagerView from "./FileManagerView.vue";
import FilePreviewDialog from "./FilePreviewDialog.vue";
import OperationErrorDialog from './OperationErrorDialog.vue';
import BrowserSkeleton from './BrowserSkeleton.vue';
import { useAppContext } from "../lib/appContext";
import { confirmAction } from "../lib/confirm";
import { canPreview, fileKind, fileKindKey, isReadableText } from "../lib/fileKinds";
import { t, locale } from "../lib/i18n";
import { formatBytes } from "../lib/format";
import { generalSettings } from "../lib/general";
import { normalizeAppError, invoke as fileInvoke } from "../lib/invoke";
import {
  MAX_PREVIEW_BYTES,
  previewFormat as detectPreviewFormat,
} from "../lib/textPreview";
import { useAction } from "../composables/useAction";

// Shared ordinary file browser. The filename is retained for compatibility with
// existing imports; protocol wiring lives in adapters, never in view templates.
defineOptions({ name: 'FileBrowser' });
const props = defineProps({
  peer: { type: Object, required: true },
  adapter: { type: Object, default: null },
  hideSidebar: { type: Boolean, default: false },
});

const app = useAppContext();
const { busy, run } = useAction();
const titlebarTarget = Boolean(typeof document !== 'undefined' && document.getElementById('titlebar-file-actions'));

const VIEW_STORAGE_KEY = props.adapter ? 'peerdav.cloudView' : 'peerdav.peerView';
const VIEW_MODES = ["icons", "list", "columns", "gallery"];
let fileSession = null;
const fileCapabilities = ref(null);
const supportsMove = ref(false);
const moveOpen = ref(false);
const moveDestination = ref('');
const moveLoading = ref(false);
async function listMoveFolders(id = '') {
  const generation = sessionGeneration;
  const session = await getFileSession();
  let next = null;
  const folders = [];
  const seen = new Set();
  do {
    const page = await fileInvoke('file_list', { sessionId: session.sessionId, id, next });
    if (browserDisposed || generation !== sessionGeneration) return [];
    folders.push(...page.entries.filter(entry => entry.isDir && !entry.protected && !selectedEntries.value.some(source => source.id === entry.id)));
    next = page.next;
    if (next && (seen.has(next) || seen.size >= 1000)) throw { code: 'FILE_LIST_LIMIT', message: t('cloudBrowser.invalidPlan') };
    if (next) seen.add(next);
  } while (next);
  return folders;
}
async function dropMove(sources, target) {
  if (!supportsMove.value || busy.value || !target.isDir) return;
  const generation = sessionGeneration;
  if (!await confirmAction(t('drive.moveConfirm', { count: sources.length, name: target.name }), t('drive.move'), { confirmText: t('drive.move') }) || browserDisposed || generation !== sessionGeneration) return;
  setSelection(sources); moveDestination.value = target.id;
  await moveSelected();
}
function showMove() { moveDestination.value = ''; moveOpen.value = true; }
async function moveSelected() {
  if (moveLoading.value || !supportsMove.value) return;
  const selected = selectedEntries.value.slice();
  const generation = sessionGeneration;
  moveLoading.value = true;
  try {
    const session = await getFileSession();
    for (const entry of selected) {
      if (browserDisposed || generation !== sessionGeneration) return;
      await fileInvoke('file_move', { sessionId: session.sessionId, id: entry.id, destination: moveDestination.value });
    }
    if (browserDisposed || generation !== sessionGeneration) return;
    moveOpen.value = false; setSelection([]); await refreshListing();
  } catch (error) { app.handleFailure(error); }
  finally { moveLoading.value = false; }
}
let sessionOpening = null;
let sessionGeneration = 0;
async function closeFileSession() {
  sessionGeneration++;
  const previous = fileSession;
  fileSession = null; fileCapabilities.value = null; sessionOpening = null;
  supportsMove.value = false; moveOpen.value = false;
  filesPrepared.value = false;
  await cancelUploadSession();
  if (previous) await fileInvoke('file_close_session', { sessionId: previous.sessionId }).catch(app.handleFailure);
}
async function getFileSession() {
  if (fileSession) return fileSession;
  if (!sessionOpening) {
    const generation = sessionGeneration;
    const location = props.adapter?.location(activeShareId.value) || { protocol: 'peerdav', peerId: props.peer.id, shareId: activeShareId.value };
    sessionOpening = fileInvoke('file_open_session', { location }).then(async session => {
      if (generation !== sessionGeneration || browserDisposed) {
        await fileInvoke('file_close_session', { sessionId: session.sessionId });
        throw { code: 'TRANSFER_CANCELED', message: '' };
      }
      fileSession = session;
      fileCapabilities.value = session.capabilities || null;
      supportsMove.value = session.supportsMove === true;
      return session;
    }).finally(() => { if (generation === sessionGeneration) sessionOpening = null; });
  }
  return sessionOpening;
}
const rootPath = computed(() => props.adapter?.root ?? '/');
function entryDisplay(entry) {
  if (!entry.isDir) return entry;
  const driveKind = entry.driveKind || (entry.name === '_peerdav_encrypt' ? 'encrypted-drive' : entry.name === '_peerdav_mirror' ? 'mirror-drive' : null);
  return driveKind ? { ...entry, driveKind, name: t(driveKind === 'encrypted-drive' ? 'cloudBrowser.encryptedDrive' : 'cloudBrowser.mirrorDrive') } : entry;
}

const peerShares = ref([]);
const activeShareId = ref("");
const peerPath = ref("/");
const peerListing = ref(null);
const navigationBreadcrumbs = ref([]);
watch(locale, () => {
  if (peerListing.value) peerListing.value = { ...peerListing.value, entries: peerListing.value.entries.map(entryDisplay) };
  columnStack.value = columnStack.value.map(column => ({ ...column, entries: column.entries.map(entryDisplay) }));
});
const peerPreview = ref(null);
const peerPreviewPath = ref("");
const previewOpen = ref(false);
const previewLimit = ref(null);
const timeColumn = computed(() => {
  if (!props.adapter) return 'modified';
  const valid = value => (typeof value === 'number' || (typeof value === 'string' && value.trim() !== '')) && Number.isFinite(Number(value)) && Number.isFinite(new Date(Number(value) * 1000).getTime());
  if (peerListing.value?.entries.some(entry => valid(entry.modified))) return 'modified';
  return peerListing.value?.entries.some(entry => valid(entry.created)) ? 'created' : '';
});
const peerEdit = ref(null);
const viewMode = ref(readViewMode());
const columnStack = ref([]);
// 分栏模式下正在取子目录的那一列：点击后必须有即时反馈，否则深层目录看起来像没响应。
const loadingColumn = ref(-1);
// 加载中只显示到被点击的那一列，右侧空位留给即将展开的新列（loading 占位在那里）。
const visibleColumns = computed(() =>
  loadingColumn.value === -1
    ? columnStack.value
    : columnStack.value.slice(0, loadingColumn.value + 1),
);
const columnViewEl = ref(null);
const fileViewEl = ref(null);
const selectedEntry = ref(null);
const selectedEntries = ref([]);
const selectionAnchor = ref(null);
const marquee = ref(null);
let marqueeStart = null;
let suppressClick = false;
let browserDisposed = false;
let uploadPollTimer;
const canMutate = computed(() => (props.adapter ? filesPrepared.value && fileCapabilities.value?.upload === true : Boolean(activeShare.value?.permissions?.canWrite)) && fileCapabilities.value?.upload !== false && !peerListing.value?.protected && !busy.value && !uploadRunning.value);
const {
  uploadDialogOpen, uploadSummary, uploadPage, uploadRunning,
  uploadBatchError, uploadPageNotice, uploadApplyRemaining, uploadResolving, uploadOverwriteAllowed,
  selectUpload, pollUpload, cancelUpload, closeUpload, resolveUploadConflict, cancelUploadSession,
} = useUploadBatch({
  adapter: {
    ...fileUploadAdapter,
    select: async (scope, mode) => {
      const token = navigationToken;
      const session = await getFileSession();
      if (browserDisposed || token !== navigationToken) return null;
      scope.sessionId = session.sessionId;
      return fileInvoke('file_select_upload', { sessionId: session.sessionId, id: scope.remoteDir ?? scope.prefix, mode, options: scope.options });
    },
  },
  captureScope: () => props.adapter?.uploadScope(activeShare.value, peerPath.value) || ({ peerId: props.peer.id, shareName: activeShare.value.name, shareId: activeShareId.value,
    remoteDir: peerPath.value, options: { respectGitignore: generalSettings.respectGitignore, patterns: generalSettings.ignorePatterns } }),
  scopeValid: (scope) => !browserDisposed && (props.adapter ? props.adapter.validScope(scope, activeShareId.value) : scope.peerId === props.peer.id && scope.shareId === activeShareId.value),
  canSelect: () => canMutate.value,
  onFailure: (cause) => app.handleFailure(cause),
  onCompleted: (summary, scope) => {
    app.setNotice("peer.uploadFinished", { count: summary.completedFiles });
    const path = scope.remoteDir ?? scope.prefix;
    if (peerPath.value === path) void loadPath(path);
  },
});
const {
  downloadDialogOpen, downloadState, downloadQueue, downloadPage, downloadRunning, downloadCanceling,
  downloadResolving, downloadApplyRemaining, downloadPageLoading, downloadSummary, selectDownload, pollDownload,
  loadDownloadPage, cancelDownloadQueue, resolveDownloadConflict, closeDownloadQueue, revealDownloadRoot, disposeDownload,
} = useDownloadQueue({
  captureScope: () => props.adapter?.downloadScope(activeShareId.value) || ({ peerId: props.peer.id, shareId: activeShareId.value }),
  scopeValid: (scope) => !browserDisposed && (props.adapter ? props.adapter.validScope(scope, activeShareId.value) : scope.peerId === props.peer.id && scope.shareId === activeShareId.value),
  backend: (scope) => props.adapter?.downloadBackend(scope) || ({ backend: "peer", ...scope }),
  select: async (_scope, selection) => {
    const token = navigationToken;
    const session = await getFileSession();
    if (browserDisposed || token !== navigationToken) return null;
    return fileInvoke('file_select_download', { sessionId: session.sessionId, selection });
  },
  onFailure: (cause) => app.handleFailure(cause),
});
onMounted(() => { uploadPollTimer = setInterval(() => { void pollUpload(); void pollDownload(); }, 1500); });
watch([() => props.peer.id, activeShareId], () => { void closeFileSession(); void cancelUploadSession(); void disposeDownload(); }, { flush: "sync" });

onBeforeUnmount(() => {
  browserDisposed = true;
  void closeFileSession();
  clearInterval(uploadPollTimer);
  clearTimeout(preparationTimer);
  void disposeDownload();
  void cancelUploadSession();
  window.removeEventListener("keydown", onModifierKeydown, true);
  window.removeEventListener("keyup", onModifierKeyup, true);
  window.removeEventListener("blur", resetHeldModifiers);
  window.removeEventListener("mousedown", onShortcutsOutsideMousedown, true);
  window.removeEventListener("keydown", onShortcutsKeydown, true);
});

const activeShare = computed(
  () => peerShares.value.find((share) => share.id === activeShareId.value) ?? null,
);

const canDeleteEntries = computed(() =>
  (props.adapter ? filesPrepared.value && fileCapabilities.value?.delete === true : Boolean(activeShare.value?.permissions?.canDelete) && fileCapabilities.value?.delete !== false),
);

const peerHost = computed(() => {
  try {
    return new URL(props.peer.url).hostname || props.peer.url;
  } catch {
    return props.peer.url;
  }
});

const breadcrumbs = computed(() => (peerListing.value?.breadcrumbs ?? navigationBreadcrumbs.value ?? []).map(crumb => ({ ...crumb, name: crumb.name === '_peerdav_encrypt' ? t('cloudBrowser.encryptedDrive') : crumb.name === '_peerdav_mirror' ? t('cloudBrowser.mirrorDrive') : crumb.name })));

const breadcrumbsEl = ref(null);

watch(
  breadcrumbs,
  () => {
    const element = breadcrumbsEl.value;
    if (element) element.scrollLeft = element.scrollWidth;
  },
  { flush: "post" },
);

function scrollBreadcrumbs(event) {
  const element = event.currentTarget;
  if (!element || element.scrollWidth <= element.clientWidth) return;
  if (Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
  event.preventDefault();
  element.scrollLeft += event.deltaY;
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

// 新列或 loading 占位列出现时滚到最右，保证反馈落在视野内而不是屏幕外。
watch(
  [columnStack, loadingColumn],
  () => {
    const element = columnViewEl.value;
    if (element) element.scrollLeft = element.scrollWidth;
  },
  { flush: "post" },
);

// 进入目录后自动聚焦文件区域，方向键无需先点击空白处即可使用；
// 焦点已在区域内或弹窗/输入框上时不抢焦点。
watch(peerListing, () => {
  void nextTick(() => {
    const view = fileViewEl.value;
    if (!view) return;
    const active = document.activeElement;
    if (active && view.contains(active)) return;
    if (
      active?.closest?.(
        ".el-overlay, .el-dialog, .el-message-box, .preview-dialog, .edit-panel",
      )
    ) {
      return;
    }
    if (
      active?.isContentEditable ||
      ["INPUT", "TEXTAREA", "SELECT"].includes(active?.tagName)
    ) {
      return;
    }
    view.focus();
  });
});

const activePreview = computed(() =>
  peerPreview.value && peerPreviewPath.value === selectedEntry.value?.id
    ? peerPreview.value
    : null,
);

const peerPreviewSource = computed(() => {
  if (!activePreview.value || activePreview.value.encoding !== "base64") {
    return "";
  }
  return `data:${activePreview.value.mime};base64,${activePreview.value.data}`;
});

const isImagePreview = computed(() =>
  Boolean(activePreview.value?.mime?.startsWith("image/")),
);

const isAudioPreview = computed(() =>
  Boolean(activePreview.value?.mime?.startsWith("audio/")),
);

const isVideoPreview = computed(() =>
  Boolean(activePreview.value?.mime?.startsWith("video/")),
);

const previewTarget = computed(() => selectedEntry.value);

const galleryFile = computed(() =>
  selectedEntry.value && !selectedEntry.value.isDir ? selectedEntry.value : null,
);

function readViewMode() {
  try {
    const stored = globalThis.localStorage?.getItem(VIEW_STORAGE_KEY);
    return VIEW_MODES.includes(stored) ? stored : "icons";
  } catch {
    return "icons";
  }
}

function setViewMode(mode) {
  if (!VIEW_MODES.includes(mode)) return;
  viewMode.value = mode;
  try {
    globalThis.localStorage?.setItem(VIEW_STORAGE_KEY, mode);
  } catch {
    // A disabled storage must not block switching views.
  }
  if (mode === "columns" && peerListing.value) {
    columnStack.value = [peerListing.value];
    void rebuildColumnStack();
  }
  if (mode === "gallery" && galleryFile.value && !activePreview.value) {
    if (canPreview(galleryFile.value)) {
      void previewEntry(galleryFile.value);
    }
  }
}

function setPeerNotice(key, params) {
  // 提示只出现在顶部并自动消失，不在页面里留下常驻横幅。
  if (key === "peer.editUploaded") app.setNotice?.(key, params);
  else app.setInfo?.(key, params);
}

function clearPreview() {
  peerPreview.value = null;
  peerPreviewPath.value = "";
  previewOpen.value = false;
  previewLimit.value = null;
}

function reset() {
  peerShares.value = [];
  activeShareId.value = "";
  peerPath.value = "/";
  peerListing.value = null;
  clearPreview();
  peerEdit.value = null;
  columnStack.value = [];
  clearSelection();
  // 切换对端时作废仍在飞行的目录请求，避免旧结果落到新对端上。
  navigationToken += 1;
  navigating.value = false;
  loadFailed.value = false;
  void disposeDownload();
  void cancelUploadSession();
}

const navigating = ref(false);
const loadFailed = ref(false);
const listingError = ref(null);
const operationError = ref(null);
const preparationError = ref(null);
const filesPrepared = ref(false);
const preparingFiles = ref(false);
const preparationHint = ref(false);
let preparationTimer;
async function prepareFiles() {
  if (!props.adapter || preparingFiles.value) return;
  const generation = sessionGeneration;
  filesPrepared.value = false;
  preparingFiles.value = true; preparationError.value = null;
  preparationTimer = setTimeout(() => { if (!browserDisposed) preparationHint.value = true; }, 300);
  try {
    const session = await getFileSession();
    await fileInvoke('file_prepare_session', { sessionId: session.sessionId });
    if (browserDisposed || generation !== sessionGeneration) return;
    filesPrepared.value = true;
  } catch (error) {
    if (!browserDisposed && generation === sessionGeneration) { preparationError.value = normalizeAppError(error); app.handleFailure(error); }
  } finally {
    clearTimeout(preparationTimer);
    if (!browserDisposed && generation === sessionGeneration) { preparingFiles.value = false; preparationHint.value = false; }
  }
}
async function createDirectory() {
  if (busy.value || (props.adapter && !filesPrepared.value) || peerListing.value?.protected || fileCapabilities.value?.createDir !== true || (!props.adapter && !activeShare.value?.permissions?.canWrite)) return;
  const token = navigationToken;
  let answer;
  try { answer = await ElMessageBox.prompt(t('peer.folderName'), t('peer.createFolder'), { confirmButtonText: t('common.confirm'), cancelButtonText: t('common.cancel'), inputValidator: value => Boolean(value?.trim()) && !/[\\/\0:]/.test(value) && !['.', '..'].includes(value) }); }
  catch (error) { if (error === 'cancel' || error === 'close') return; throw error; }
  if (browserDisposed || token !== navigationToken) return;
  await run(async () => {
    const session = await getFileSession();
    const entry = await fileInvoke('file_create_directory', { sessionId: session.sessionId, id: peerPath.value, name: answer.value });
    if (browserDisposed || token !== navigationToken) return;
    const displayed = entryDisplay({ ...entry, ...(props.adapter ? { key: entry.id } : {}) });
    const insert = page => page?.id === entry.parentId ? { ...page, entries: [...page.entries.filter(item => item.id !== entry.id), displayed].sort((a, b) => Number(b.isDir) - Number(a.isDir)) } : page;
    peerListing.value = insert(peerListing.value);
    columnStack.value = columnStack.value.map(insert);
    app.setNotice('peer.folderCreated', { name: entry.name });
  });
}

async function deleteCurrentFolder() {
  if (!props.adapter || !canDeleteEntries.value || busy.value || peerListing.value?.protected) return;
  if (peerPath.value === rootPath.value) { operationError.value = { code: 'CLOUD_DELETE_ROOT_FORBIDDEN', message: t('cloudBrowser.deleteRootForbidden') }; return; }
  const currentName = breadcrumbs.value.find(crumb => crumb.id === peerPath.value)?.name || t('peer.root');
  await deleteEntry({ id: peerPath.value, name: currentName, isDir: true });
}
let navigationToken = 0;

function navigationStorageKey() { return `peerdav.fileLocation:${props.peer.id}:${activeShareId.value}`; }
function savedNavigation() {
  try { const value = JSON.parse(globalThis.localStorage?.getItem(navigationStorageKey()) || 'null'); return Array.isArray(value) && value.length <= 128 && value.every(id => typeof id === 'string') ? value : []; } catch { return []; }
}
function applyListing(listing) {
  peerListing.value = listing;
  peerPath.value = listing.id;
  if (props.adapter) {
    try { globalThis.localStorage?.setItem(navigationStorageKey(), JSON.stringify([...(listing.breadcrumbs || []).map(crumb => crumb.id), listing.id].filter((id, index, ids) => ids.indexOf(id) === index))); } catch { /* 存储受限不影响导航。 */ }
  }
  clearSelection();
  clearPreview();
  if (viewMode.value === "columns") {
    columnStack.value = [listing];
  }
}

async function fetchListing(path, next = null) {
  const session = await getFileSession();
  const page = await fileInvoke('file_list', { sessionId: session.sessionId, id: path, next });
  if (props.adapter && !page.complete && page.entries.length) app.setInfo?.('cloudBrowser.incomplete');
  return { ...page, entries: page.entries.map(entry => entryDisplay({ ...entry, ...(props.adapter ? { key: entry.id } : {}) })).sort((a, b) => Number(b.isDir) - Number(a.isDir)), shareId: activeShareId.value, truncated: !page.complete };
}
async function loadMore() {
  if (!peerListing.value?.next || navigating.value) return;
  const token = navigationToken;
  const previous = peerListing.value;
  navigating.value = true;
  try {
    const page = await fetchListing(previous.id, previous.next);
    if (token !== navigationToken) return;
    // A later provider page may contain directories: regroup the merged list.
    peerListing.value = { ...page, entries: [...new Map([...previous.entries, ...page.entries].map(entry => [entry.id, entry])).values()].sort((a, b) => Number(b.isDir) - Number(a.isDir)) };
  } catch (error) { if (token === navigationToken) app.handleFailure(error); }
  finally { if (token === navigationToken) navigating.value = false; }
}

async function rebuildColumnStack() {
  const current = peerListing.value;
  if (!current) return;
  const targetPath = peerPath.value;
  const token = navigationToken;
  const levels = breadcrumbs.value.map(crumb => crumb.id === targetPath ? current : { id: crumb.id, entries: [], loading: true, complete: false });
  columnStack.value = levels;
  await Promise.all(levels.map(async (level, index) => {
    if (level === current) return;
    try {
      const listing = await fetchListing(level.id);
      if (browserDisposed || token !== navigationToken || viewMode.value !== 'columns' || peerPath.value !== targetPath) return;
      columnStack.value[index] = listing;
    } catch (error) {
      if (browserDisposed || token !== navigationToken || viewMode.value !== 'columns' || peerPath.value !== targetPath) return;
      columnStack.value[index] = { ...level, loading: false, failed: true };
      operationError.value = normalizeAppError(error);
      app.handleFailure(error);
    }
  }));
}

// 进入目录时先更新地址栏并清空文件区放上 loading，让"已进入"立刻可见；
// 令牌保证用户中途再导航时，旧请求返回的内容不会覆盖新目录。
async function loadPath(path) {
  const token = ++navigationToken;
  const knownTrail = breadcrumbs.value;
  const ancestor = knownTrail.findIndex(crumb => crumb.id === path);
  const entry = peerListing.value?.entries.find(entry => entry.id === path && entry.isDir);
  navigationBreadcrumbs.value = ancestor >= 0 ? knownTrail.slice(0, ancestor + 1) : entry ? [...knownTrail, { id: entry.id, name: entry.name }] : [];
  peerPath.value = path;
  peerListing.value = null;
  loadFailed.value = false;
  listingError.value = null;
  clearSelection();
  clearPreview();
  navigating.value = true;
  try {
    const listing = await fetchListing(path);
    if (token !== navigationToken) return;
    applyListing(listing);
    if (viewMode.value === "columns") {
      await rebuildColumnStack();
    }
  } catch (error) {
    if (token !== navigationToken) return;
    loadFailed.value = true;
    listingError.value = normalizeAppError(error);
    app.handleFailure(error);
  } finally {
    if (token === navigationToken) navigating.value = false;
  }
}

async function loadPeer() {
  reset();
  await run(async () => {
    peerShares.value = props.adapter ? await props.adapter.roots() : await app.invoke("list_peer_shares", {
      peerId: props.peer.id,
    });
    if (peerShares.value.length) {
      activeShareId.value = peerShares.value[0].id;
      await getFileSession();
      void prepareFiles();
      const saved = props.adapter ? savedNavigation() : [];
      await loadPath(rootPath.value);
      let restoreToken = navigationToken;
      for (const id of saved.filter(id => id !== rootPath.value)) {
        if (browserDisposed || restoreToken !== navigationToken) break;
        if (!peerListing.value?.entries.some(entry => entry.id === id && entry.isDir)) break;
        await loadPath(id);
        restoreToken += 1;
      }
    }
  });
}

async function openShare(shareId) {
  if (!props.peer.id) return;
  activeShareId.value = shareId;
  void loadPath(rootPath.value);
}

// 导航不再被 busy 阻塞：用户可以在下载等操作进行中继续切换目录，
// 竞态由 loadPath 内的令牌保证。
async function browse(path) {
  if (!props.peer.id || !activeShareId.value) return;
  void loadPath(path);
}

async function refreshListing() {
  if (!activeShareId.value) return;
  if (!peerListing.value) { void loadPath(peerPath.value); return; }
  const token = ++navigationToken;
  const path = peerPath.value;
  clearSelection(); clearPreview();
  navigating.value = true;
  try {
    const listing = await fetchListing(path);
    if (token !== navigationToken || browserDisposed) return;
    applyListing(listing);
    operationError.value = null;
    if (viewMode.value === 'columns') await rebuildColumnStack();
  } catch (error) {
    if (token === navigationToken && !browserDisposed) { operationError.value = normalizeAppError(error); app.handleFailure(error); }
  } finally { if (token === navigationToken) navigating.value = false; }
}

function isSelected(entry) {
  return selectedEntries.value.some((item) => item.id === entry?.id);
}

function setSelection(entries) {
  const list = entries.filter(Boolean);
  selectedEntries.value = list;
  selectedEntry.value = list.length ? list[list.length - 1] : null;
}

function clearSelection() {
  selectionAnchor.value = null;
  setSelection([]);
}

// 修饰键判定不能只依赖 click 事件：部分 WebKit 环境（Tauri 在 Linux 用的 WebKitGTK）
// 在 mousedown 被 preventDefault 后不会把 Ctrl/Cmd/Alt/Shift 带进 click，
// 真机上表现为 ctrl+点击退化成单选。这里同时采信 mousedown 的修饰键和键盘实时状态，
// 任一来源为真即按多选处理；键盘导航等内部调用走 selectSingle，不受修饰键影响。
// 附加选择键：Ctrl / Cmd / Alt 三者等价；Shift 仍只做范围选择。
const pointerModifiers = { ctrl: false, meta: false, alt: false, shift: false };
const heldModifiers = { ctrl: false, meta: false, alt: false, shift: false };

function markModifierFlag(target, event, active) {
  if (event.key === "Control" || event.code === "ControlLeft" || event.code === "ControlRight") {
    target.ctrl = active;
  } else if (event.key === "Meta" || event.code === "MetaLeft" || event.code === "MetaRight") {
    target.meta = active;
  } else if (event.key === "Alt" || event.code === "AltLeft" || event.code === "AltRight") {
    target.alt = active;
  } else if (event.key === "Shift" || event.code === "ShiftLeft" || event.code === "ShiftRight") {
    target.shift = active;
  }
}

function onModifierKeydown(event) {
  markModifierFlag(heldModifiers, event, true);
}

function onModifierKeyup(event) {
  markModifierFlag(heldModifiers, event, false);
}

function resetHeldModifiers() {
  heldModifiers.ctrl = false;
  heldModifiers.meta = false;
  heldModifiers.alt = false;
  heldModifiers.shift = false;
}

window.addEventListener("keydown", onModifierKeydown, true);
window.addEventListener("keyup", onModifierKeyup, true);
window.addEventListener("blur", resetHeldModifiers);

function selectSingle(entry) {
  selectionAnchor.value = entry.id;
  setSelection([entry]);
  if (viewMode.value === "gallery" && !entry.isDir && canPreview(entry)) {
    void previewEntry(entry);
  }
}

function selectEntry(entry, event) {
  if (suppressClick) return;
  const additive = Boolean(
    event?.ctrlKey ||
      event?.metaKey ||
      event?.altKey ||
      pointerModifiers.ctrl ||
      pointerModifiers.meta ||
      pointerModifiers.alt ||
      heldModifiers.ctrl ||
      heldModifiers.meta ||
      heldModifiers.alt,
  );
  const ranged = Boolean(event?.shiftKey || pointerModifiers.shift || heldModifiers.shift);
  // 指针修饰键只对本次点击有效，避免残留影响后续键盘触发的选择。
  pointerModifiers.ctrl = false;
  pointerModifiers.meta = false;
  pointerModifiers.alt = false;
  pointerModifiers.shift = false;
  const result = selectionForClick({ entries: peerListing.value?.entries ?? [], selection: selectedEntries.value,
    anchor: selectionAnchor.value, entry, identityField: "id", additive, ranged });
  if (result) {
    setSelection(result.selection);
    selectionAnchor.value = result.anchor;
    return;
  }
  selectSingle(entry);
}

// 任意位置按下并拖动都能框选（包括从文件上起拉），轻点空白处清空选择。
// preventDefault 关闭原生拖动：避免文字选择和"从下往上拉时区域自动向下滚动"。
function onFileViewMousedown(event) {
  // 先记下按下时的修饰键，click 事件万一丢掉修饰位还能靠它判定多选。
  pointerModifiers.ctrl = Boolean(event.ctrlKey);
  pointerModifiers.meta = Boolean(event.metaKey);
  pointerModifiers.alt = Boolean(event.altKey);
  pointerModifiers.shift = Boolean(event.shiftKey);
  if (event.button !== 0 || viewMode.value === "columns") return;
  // 按住 Ctrl/Cmd/Shift/Alt 时不能启动框选：框选会覆盖当前选择并抑制随后的 click，
  // 真机上表现为 ctrl/cmd 跳跃多选完全失效。这类按下交给 click 逻辑处理。
  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  if (event.target?.closest?.("a, input, textarea, select, [contenteditable], pre, video, audio")) {
    return;
  }
  if (event.target?.closest?.('[draggable="true"]')) return;
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
  // 微小抖动仍视为点击，不进入框选。
  if (width <= 3 && height <= 3) return;
  marquee.value = {
    left: Math.min(marqueeStart.clientX, event.clientX),
    top: Math.min(marqueeStart.clientY, event.clientY),
    width,
    height,
  };
  const band = marquee.value;
  const hits = [];
  for (const element of fileViewEl.value?.querySelectorAll("[data-path]") ?? []) {
    const rect = element.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    if (
      rect.left < band.left + band.width &&
      rect.right > band.left &&
      rect.top < band.top + band.height &&
      rect.bottom > band.top
    ) {
      hits.push(element.getAttribute("data-path"));
    }
  }
  const listing = peerListing.value?.entries ?? [];
  setSelection(listing.filter((entry) => hits.includes(entry.id)));
}

function onMarqueeUp(event) {
  window.removeEventListener("mousemove", onMarqueeMove);
  window.removeEventListener("mouseup", onMarqueeUp);
  const band = marquee.value;
  const dragged = Boolean(band) && (band.width > 3 || band.height > 3);
  if (dragged) {
    // 拖动后的 click 会落在起点条目上，必须忽略，否则多选会被折叠成单选。
    suppressClick = true;
    setTimeout(() => {
      suppressClick = false;
    }, 0);
  } else if (!event.target?.closest?.("[data-path]")) {
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

function openEntry(entry) {
  if (entry.isDir) {
    void browse(entry.id);
  } else if (canPreview(entry)) {
    void previewEntry(entry);
  }
}

// 画廊用左右方向键、其余视图用上下方向键在可见条目间移动选中项，并循环切换：
// 首个按上/左跳到末尾，末尾按下/右跳到首个。
function navigateSelection(delta) {
  const entries = peerListing.value?.entries ?? [];
  const length = entries.length;
  if (!length) return;
  const current = entries.findIndex((entry) => entry.id === selectedEntry.value?.id);
  const index =
    current === -1
      ? delta > 0
        ? 0
        : length - 1
      : (current + delta + length) % length;
  selectSingle(entries[index]);
  void nextTick(() => {
    fileViewEl.value?.querySelector(".is-selected")?.focus?.();
  });
}

// 返回上一级目录。通用快捷键是 Alt+↑（Windows 资源管理器、GNOME Files），
// macOS Finder 用 Cmd+↑，因此两个修饰键都支持。根目录不做任何操作。
function goUpLevel() {
  if (!activeShareId.value || busy.value) return;
  const crumbs = breadcrumbs.value;
  if (crumbs.length <= 1) return;
  void browse(crumbs[crumbs.length - 2].id);
}

// 快捷键说明面板：只列本视图真实实现的行为。修饰键标签按平台精确列出，
// 跳跃多选接受 Ctrl / ⌘ / Alt，返回上一级只接受 Alt + ↑（macOS 额外支持 ⌘ + ↑）。
// 面板自己渲染并 Teleport 到 body：titlebar 里的锚点用库的 popper 定位不可控，
// 且需要点击外部/Esc 立即收起，直接受控更可靠。
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
  if (shortcutsOpen.value) {
    closeShortcutsPanel();
    return;
  }
  shortcutsOpen.value = true;
  void nextTick(positionShortcutsPanel);
}

function onShortcutsOutsideMousedown(event) {
  if (!shortcutsOpen.value) return;
  const target = event.target;
  if (shortcutsPanelEl.value?.contains?.(target) || shortcutsTriggerEl.value?.contains?.(target)) {
    return;
  }
  closeShortcutsPanel();
}

function onShortcutsKeydown(event) {
  if (event.key === "Escape" && shortcutsOpen.value) closeShortcutsPanel();
}

window.addEventListener("mousedown", onShortcutsOutsideMousedown, true);
window.addEventListener("keydown", onShortcutsKeydown, true);

function onFileViewKeydown(event) {
  if (event.key === "ArrowUp" && (event.altKey || event.metaKey)) {
    const target = event.target;
    if (target?.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName)) {
      return;
    }
    event.preventDefault();
    goUpLevel();
    return;
  }
  const gallery = viewMode.value === "gallery";
  const forward = gallery ? "ArrowRight" : "ArrowDown";
  const back = gallery ? "ArrowLeft" : "ArrowUp";
  if (event.key !== forward && event.key !== back) return;
  const target = event.target;
  if (target?.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName)) {
    return;
  }
  event.preventDefault();
  navigateSelection(event.key === forward ? 1 : -1);
}

async function loadColumn(entry, index, event) {
  if (event?.detail > 1) return;
  if (event && (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey)) { selectEntry(entry, event); return; }
  if (!entry.isDir) {
    selectSingle(entry);
    return;
  }
  const token = ++navigationToken;
  loadingColumn.value = index;
  peerPath.value = entry.id;
  clearSelection(); clearPreview();
  try {
    const listing = await fetchListing(entry.id);
    if (browserDisposed || token !== navigationToken) return;
    peerListing.value = listing;
    peerPath.value = listing.id;
    setSelection([entry]);
    columnStack.value = [...columnStack.value.slice(0, index + 1), listing];
  } catch (error) {
    if (!browserDisposed && token === navigationToken) app.handleFailure(error);
  } finally {
    if (token === navigationToken) loadingColumn.value = -1;
  }
}

function downloadEntry(entry) {
  if (!entry || entry.isDir || busy.value || fileCapabilities.value?.download === false) return;
  return selectDownload({ mode: "single", id: entry.id });
}
function downloadDirectory(entry) {
  if (!entry?.isDir || busy.value || fileCapabilities.value?.download === false || fileCapabilities.value?.downloadDirectory === false) return;
  return selectDownload({ mode: "folder", id: entry.id });
}
function downloadBatch(entries) {
  if (!entries.length || busy.value || fileCapabilities.value?.download === false || (entries.some(entry => entry.isDir) && fileCapabilities.value?.downloadDirectory === false)) return;
  return selectDownload({ mode: "files", ids: entries.map((entry) => entry.id) });
}

function runSelectionAction(action) {
  const entries = selectedEntries.value.slice();
  clearSelection();
  if (entries.length) return action(entries);
}

function downloadSelected(entries = selectedEntries.value) {
  if (!entries.length || busy.value || fileCapabilities.value?.download === false || (entries.some(entry => entry.isDir) && fileCapabilities.value?.downloadDirectory === false)) return;
  if (entries.length === 1) {
    const entry = entries[0];
    if (entry.isDir) void downloadDirectory(entry);
    else void downloadEntry(entry);
    return;
  }
  void downloadBatch(entries);
}

function chooseUploadFiles() {
  if (props.adapter) return selectUpload('single');
  return selectUpload('files');
}
function chooseUploadFolder() { return selectUpload("folder"); }

function previewUsesDialog(entry, preview) {
  if (viewMode.value !== "gallery") return true;
  const mime = String(preview?.mime ?? "");
  if (
    mime.startsWith("image/") ||
    mime.startsWith("audio/") ||
    mime.startsWith("video/")
  ) {
    return false;
  }
  return ["markdown", "table", "code", "text"].includes(
    detectPreviewFormat(entry),
  );
}

function closePreview() {
  clearPreview();
}

async function previewEntry(entry) {
  if (!entry || entry.isDir || busy.value || !canPreview(entry) || fileCapabilities.value?.preview === false) return;
  clearPreview();
  setSelection([entry]);

  if (Number(entry.size) > MAX_PREVIEW_BYTES) {
    peerPreviewPath.value = entry.id;
    previewLimit.value = {
      code: "PEER_PREVIEW_TOO_LARGE",
      size: Number(entry.size),
      limit: MAX_PREVIEW_BYTES,
    };
    previewOpen.value = true;
    return;
  }

  await run(async () => {
    try {
        const session = await getFileSession();
        const preview = await fileInvoke('file_preview', { sessionId: session.sessionId, id: entry.id });
      peerPreview.value = preview;
      peerPreviewPath.value = entry.id;
      previewOpen.value = previewUsesDialog(entry, preview);
    } catch (error) {
      if (
        error?.code === "PEER_PREVIEW_TOO_LARGE" ||
        error?.code === "PEER_PREVIEW_TOO_MANY_LINES"
      ) {
        peerPreviewPath.value = entry.id;
        previewLimit.value = { code: error.code, ...(error.context ?? {}) };
        previewOpen.value = true;
        return;
      }
      throw error;
    }
  });
}

async function onlinePreviewEntry(entry) {
  await run(async () => {
    const session = await getFileSession();
    await fileInvoke('file_online_preview', { sessionId: session.sessionId, id: entry.id });
  });
}

async function editEntry(entry) {
  if (!entry || entry.isDir || busy.value) return;
  if (peerEdit.value) {
    setPeerNotice('peer.editAlreadyOpen');
    return;
  }
  await run(async () => {
    const session = await getFileSession();
    const edit = await fileInvoke('file_prepare_edit', { sessionId: session.sessionId, id: entry.id });
    peerEdit.value = edit;
    setPeerNotice('peer.cloudEditOpened');
  });
}

function applyDeletion(entry, result) {
  if (!peerListing.value) return;
  const deleted = new Set(result?.deleted || []);
  const failed = result?.failed || [];
  const removedTarget = failed.length === 0 && deleted.size > 0;
  peerListing.value = { ...peerListing.value, entries: peerListing.value.entries.filter(item => {
    if (deleted.has(item.id)) return false;
    return !(removedTarget && item.id === entry.id);
  }) };
}

async function deleteSelected(entries = selectedEntries.value) {
  if (!entries.length || busy.value || !canDeleteEntries.value || peerListing.value?.protected) return;
  if (entries.some(entry => entry.id === rootPath.value)) return;
  const targets = [...new Map(entries.map(entry => [entry.id, entry])).values()];
  if (targets.some(entry => entry.protected || entry.id === rootPath.value)) return;
  const token = navigationToken;
  await run(async () => {
    const confirmed = await confirmAction(t('peer.deleteSelectedConfirm', { count: targets.length, names: targets.map(entry => entry.name).join('\n') }), t('common.delete'), { danger: true, confirmText: t('common.delete') });
    if (!confirmed || browserDisposed || token !== navigationToken) return;
    const session = await getFileSession();
    let success = 0;
    const failures = [];
    for (const entry of targets) {
      if (browserDisposed || token !== navigationToken) return;
      try {
        let result;
        if (props.adapter) result = await props.adapter.remove(entry, () => !browserDisposed && token === navigationToken, session.sessionId, true);
        else {
          const plan = await fileInvoke('file_delete_plan', { sessionId: session.sessionId, id: entry.id });
          if (browserDisposed || token !== navigationToken) return;
          result = await fileInvoke('file_delete_execute', { sessionId: session.sessionId, planId: plan.planId, confirmed: true });
        }
        if (result === false) return;
        if (browserDisposed || token !== navigationToken) return;
        applyDeletion(entry, result);
        if (result.failed?.length) failures.push(...result.failed.map(item => item.error));
        else if (result.deleted?.length) success++;
      } catch (error) { failures.push(normalizeAppError(error)); }
    }
    if (browserDisposed || token !== navigationToken) return;
    if (failures.length) app.setError('cloudBrowser.deletePartial', { deleted: success, failed: failures.length, reason: failures[0]?.message });
    else app.setNotice('cloudBrowser.deletedObjects', { count: success });
  });
}

async function deleteEntry(entry) {
  if (!entry || entry.protected || peerListing.value?.protected || busy.value || !canDeleteEntries.value) return;
  await run(async () => {
    // 禁止删除共享根目录（相当于 rm -rf /）：其中的条目必须逐个提交删除。
    if (entry.id === rootPath.value) {
      throw { code: "PEER_DELETE_ROOT_FORBIDDEN", message: t("peer.deleteRootForbidden"), detail: null, context: { path: entry.id } };
    }
    if (props.adapter) {
      const token = navigationToken;
      try {
        const session = await getFileSession();
        const result = await props.adapter.remove(entry, () => !browserDisposed && token === navigationToken, session.sessionId);
        if (result === false || browserDisposed || token !== navigationToken) return;
        applyDeletion(entry, result);
        if (result.failed?.length) {
          if (!result.deleted?.length) throw result.failed[0].error;
        } else app.setNotice('peer.deleted', { name: entry.name });
      } catch (error) {
        if (!browserDisposed && token === navigationToken) operationError.value = normalizeAppError(error);
        throw error;
      }
      return;
    }
    const token = navigationToken;
    const confirmed = await confirmAction(
      t(entry.isDir ? "peer.deleteFolderConfirm" : "peer.deleteFileConfirm", {
        name: entry.name,
      }),
      t("common.delete"),
      { danger: true, confirmText: t("common.delete") },
    );
    if (!confirmed || browserDisposed || token !== navigationToken) return;
    const session = await getFileSession();
    const plan = await fileInvoke('file_delete_plan', { sessionId: session.sessionId, id: entry.id });
    if (browserDisposed || token !== navigationToken) return;

    if (peerEdit.value?.id === entry.id) {
      await fileInvoke('file_discard_edit', {
        editId: peerEdit.value.editId,
      });
      peerEdit.value = null;
    }
    if (browserDisposed || token !== navigationToken) return;
    const result = await fileInvoke('file_delete_execute', { sessionId: session.sessionId, planId: plan.planId, confirmed: true });
    if (browserDisposed || token !== navigationToken) return;
    applyDeletion(entry, result);
    if (result.failed?.length) throw result.failed[0].error;
    app.setNotice("peer.deleted", { name: entry.name });
  });
}

async function commitPeerEdit() {
  if (!peerEdit.value || busy.value) return;
  await run(async () => {
    const transferred = await fileInvoke('file_commit_edit', { editId: peerEdit.value.editId });
    setPeerNotice(transferred ? 'peer.editUploaded' : 'peer.editNoChange', { path: peerEdit.value.name });
    await loadPath(peerPath.value);
  });
}

async function discardPeerEdit() {
  if (!peerEdit.value || busy.value) return;
  await run(async () => {
    await fileInvoke('file_discard_edit', { editId: peerEdit.value.editId });
    peerEdit.value = null;
    setPeerNotice("peer.editDiscarded");
  });
}

function entryKind(entry) {
  return t(fileKindKey(fileKind(entry)));
}

function formatSize(entry) {
  if (!entry || entry.isDir) return "—";
  return formatBytes(entry.size);
}

function formatModified(seconds) {
  if (seconds == null || seconds === '' || typeof seconds === 'boolean' || (typeof seconds === 'string' && !seconds.trim())) return "—";
  try {
    return new Intl.DateTimeFormat(document.documentElement.lang || undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(Number(seconds) * 1000));
  } catch {
    return "—";
  }
}

defineExpose({ refresh: refreshListing });
watch(() => props.peer.id, loadPeer, { immediate: true });
</script>

<template>
  <div class="peer-browser" :class="{ 'peer-browser--embedded': hideSidebar }">
    <div class="browser-body">
      <aside v-if="!hideSidebar" class="browser-sidebar">
        <div class="sidebar-identity">
          <strong>{{ peer.label }}</strong>
          <span>{{ peerHost }}</span>
        </div>
        <p class="sidebar-label">{{ t("peer.shares") }}</p>
        <button
          v-for="share in peerShares"
          :key="share.id"
          type="button"
          class="share-link"
          :class="{ 'is-active': share.id === activeShareId }"
          @click="openShare(share.id)"
        >
          <FileGlyph
            :entry="{ isDir: true, name: share.name, contentType: null }"
          />
          <span>{{ share.name }}</span>
        </button>
        <p v-if="!peerShares.length" class="sidebar-empty">
          {{ t("peer.emptyDir") }}
        </p>
      </aside>

      <main class="browser-main" :class="{ 'browser-main--selection': selectedEntries.length }">
        <OperationErrorDialog v-if="preparationError" :error="preparationError" retryable recheck :busy="preparingFiles" @retry="prepareFiles" @close="preparationError = null" />
        <OperationErrorDialog :error="operationError" retryable @retry="viewMode === 'columns' ? rebuildColumnStack() : refreshListing()" @close="operationError = null" />
        <div class="pathbar">
          <BrowserSkeleton v-if="preparationHint" class="pathbar-status" compact />
          <div
            ref="breadcrumbsEl"
            class="breadcrumbs"
            aria-label="Breadcrumb"
            @wheel="scrollBreadcrumbs"
          >
            <template v-for="(crumb, index) in breadcrumbs" :key="crumb.id">
              <span v-if="index" class="breadcrumb-sep">›</span>
              <button
                type="button"
                class="breadcrumb"
                :class="{ 'is-current': index === breadcrumbs.length - 1 }"
                @click="browse(crumb.id)"
              >
                {{ crumb.name }}
              </button>
            </template>
          </div>
        </div>

        <Teleport to="#titlebar-file-actions" :disabled="!titlebarTarget">
          <div class="pathbar-actions">
            <div class="view-switch" role="group" :aria-label="t('peer.viewMode')">
              <el-tooltip
                v-for="mode in VIEW_MODES"
                :key="mode"
                :content="t(`peer.${mode}`)"
                placement="bottom"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button
                  type="button"
                  class="view-switch__button"
                  :class="{ 'is-active': viewMode === mode }"
                  :aria-label="t(`peer.${mode}`)"
                  :aria-pressed="viewMode === mode"
                  @click="setViewMode(mode)"
                >
                  <svg v-if="mode === 'icons'" viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="4" y="4" width="6" height="6" rx="1" />
                    <rect x="14" y="4" width="6" height="6" rx="1" />
                    <rect x="4" y="14" width="6" height="6" rx="1" />
                    <rect x="14" y="14" width="6" height="6" rx="1" />
                  </svg>
                  <svg v-else-if="mode === 'list'" viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M4 6h16M4 12h16M4 18h16" />
                  </svg>
                  <svg
                    v-else-if="mode === 'columns'"
                    viewBox="0 0 24 24"
                    aria-hidden="true"
                  >
                    <rect x="3.5" y="4.5" width="5" height="15" rx="1" />
                    <rect x="9.5" y="4.5" width="5" height="15" rx="1" />
                    <rect x="15.5" y="4.5" width="5" height="15" rx="1" />
                  </svg>
                  <svg v-else viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="3.5" y="4.5" width="17" height="11" rx="1.5" />
                    <rect x="5.5" y="17.5" width="4" height="3" rx="0.8" />
                    <rect x="10.5" y="17.5" width="4" height="3" rx="0.8" />
                    <rect x="15.5" y="17.5" width="3" height="3" rx="0.8" />
                  </svg>
                </button>
              </el-tooltip>
            </div>
            <span class="titlebar-divider" aria-hidden="true"></span>
            <div class="view-switch" role="group" :aria-label="t('peer.fileActions')">
              <el-tooltip v-if="fileCapabilities?.createDir && (adapter || activeShare?.permissions?.canWrite)" :content="t('peer.createFolder')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="view-switch__button" data-test="create-directory" :aria-label="t('peer.createFolder')" :disabled="(adapter && !filesPrepared) || busy || peerListing?.protected || uploadRunning" @click="createDirectory"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" /><path d="M12 11v6M9 14h6" /></svg></button>
              </el-tooltip>
              <el-tooltip
                :content="t('peer.uploadFiles')"
                placement="bottom"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button
                  type="button"
                  class="view-switch__button"
                  :aria-label="t('peer.uploadFiles')"
                  data-test="upload"
                  :disabled="!canMutate"
                  @click="chooseUploadFiles"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M12 16V4" />
                    <path d="m8 8 4-4 4 4" />
                    <path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" />
                  </svg>
                </button>
              </el-tooltip>
              <el-tooltip
                :content="t('peer.uploadFolder')"
                placement="bottom"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button
                  type="button"
                  class="view-switch__button"
                  :aria-label="t('peer.uploadFolder')"
                  data-test="upload-directory"
                  :disabled="!canMutate"
                  @click="chooseUploadFolder"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" />
                    <path d="M12 17v-5M9.5 14.5 12 12l2.5 2.5" />
                  </svg>
                </button>
              </el-tooltip>
              <span class="titlebar-divider" aria-hidden="true"></span>
              <el-tooltip v-if="adapter && fileCapabilities?.delete && !peerListing?.protected" :content="t('cloudBrowser.deletePrefix')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="view-switch__button" data-test="delete-prefix" :aria-label="t('cloudBrowser.deletePrefix')" :disabled="!canDeleteEntries || busy || uploadRunning || peerListing?.protected || peerPath === rootPath || peerListing?.entries.some(entry => entry.protected)" @click="deleteCurrentFolder"><svg v-if="busy" class="action-spinner" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8" fill="none" stroke="currentColor" stroke-width="2" stroke-dasharray="34 20" /></svg><svg v-else viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V5h6v2M6 7l1 12h10l1-12" /></svg></button>
              </el-tooltip>
              <el-tooltip :content="t('peer.refresh')" placement="bottom" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="view-switch__button" :aria-label="t('peer.refresh')" data-test="refresh" :disabled="busy || !activeShareId" @click="refreshListing"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 11a8 8 0 1 0-2.3 5.7" /><path d="M20 5v6h-6" /></svg></button>
              </el-tooltip>
              <span class="titlebar-divider" aria-hidden="true"></span>
              <button
                ref="shortcutsTriggerEl"
                type="button"
                class="view-switch__button shortcuts-trigger"
                :aria-label="t('peer.shortcuts')"
                :aria-expanded="shortcutsOpen ? 'true' : 'false'"
                @click="toggleShortcutsPanel"
              >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <rect x="2.5" y="6.5" width="19" height="11" rx="2" />
                  <path d="M6 10h1.5M9.5 10h1.5M13 10h1.5M16.5 10h1.5" />
                  <path d="M6 13h1.5M9.5 13h1.5M13 13h1.5M16.5 13h1.5" />
                  <path d="M8.5 15.5h7" />
                </svg>
              </button>
              <Teleport to="body">
                <div
                  v-if="shortcutsOpen"
                  ref="shortcutsPanelEl"
                  class="shortcuts-panel"
                  :style="shortcutsPanelStyle"
                  role="dialog"
                  :aria-label="t('peer.shortcutsTitle')"
                >
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
            </div>
          </div>
        </Teleport>


        <div
          v-if="peerListing"
          ref="fileViewEl"
          class="file-view"
          tabindex="0"
          @keydown="onFileViewKeydown"
          @mousedown="onFileViewMousedown"
        >
          <div v-if="marquee" class="marquee" :style="marqueeStyle"></div>
          <FileManagerView :move-enabled="!!adapter && supportsMove && !busy && !moveLoading" @move="dropMove" identity-field="id" column-identity-field="id" :entries="peerListing.entries" :mode="viewMode" :selection="selectedEntries"
            :columns="visibleColumns" :column-loading="loadingColumn !== -1" :append-loading-column="loadingColumn !== -1"
            :empty-label="adapter ? t('cloudBrowser.empty') : ''"
            :partial="!!peerListing.truncated" :time-column="timeColumn" :time-label="t(timeColumn === 'created' ? 'cloudBrowser.created' : 'peer.modified')" :entry-kind="entryKind" :format-size="formatSize" :format-modified="formatModified"
            @select="selectEntry" @activate="openEntry" @column-select="(entry, event, column, index) => loadColumn(entry, index, event)"
            @preview="previewEntry" @column-element="columnViewEl = $event" @column-wheel="scrollColumnView">
            <template #gallery-preview>
              <template v-if="galleryFile">
                <img
                  v-if="isImagePreview && peerPreviewSource"
                  :src="peerPreviewSource"
                  :alt="t('peer.previewAlt')"
                  class="gallery-image"
                />
                <audio
                  v-else-if="isAudioPreview && peerPreviewSource"
                  :src="peerPreviewSource"
                  class="gallery-audio"
                  controls
                  preload="metadata"
                ></audio>
                <video
                  v-else-if="isVideoPreview && peerPreviewSource"
                  :src="peerPreviewSource"
                  class="gallery-video"
                  controls
                  playsinline
                  preload="metadata"
                ></video>
                <pre
                  v-else-if="activePreview && activePreview.encoding === 'utf8'"
                  class="gallery-text"
                >{{ activePreview.data }}</pre>
                <div v-else class="gallery-placeholder">
                  <FileGlyph :entry="galleryFile" size="xl" />
                  <strong>{{ galleryFile.name }}</strong>
                  <span>{{ entryKind(galleryFile) }} · {{ formatSize(galleryFile) }}</span>
                  <el-button
                    v-if="canPreview(galleryFile)"
                    type="primary"
                    plain
                    :disabled="busy"
                    @click="previewEntry(galleryFile)"
                  >
                    {{ t("peer.preview") }}
                  </el-button>
                </div>
              </template>
              <div v-else class="gallery-placeholder">
                <FileGlyph
                  :entry="{ isDir: true, name: '', contentType: null }"
                  size="xl"
                />
                <strong>{{ t("peer.selectPreview") }}</strong>
                <span>{{ t("peer.selectPreviewHint") }}</span>
              </div>
            </template>
            <template #pagination><el-button v-if="peerListing.next" data-test="more" :disabled="navigating" @click="loadMore">{{ t('cloudBrowser.more') }}</el-button></template>
          </FileManagerView>
        </div>

        <!-- 加载中先清空旧内容并给出即时反馈；失败时提示可点击地址栏重试。 -->
        <div v-else class="file-view file-view--placeholder" :aria-busy="navigating">
          <FileManagerView v-if="navigating" :mode="viewMode" loading :entry-kind="entryKind" :format-size="formatSize" />
          <div v-else class="file-empty"><p role="alert">{{ listingError?.message || t('peer.loadFailed') }}</p><el-button data-test="retry-load" @click="loadPath(peerPath)">{{ t('peer.refresh') }}</el-button></div>
        </div>

        <div v-if="selectedEntries.length" class="selection-bar">
          <div class="selection-actions">
            <template v-if="selectedEntries.length === 1 && selectedEntry.isDir">
              <el-tooltip v-if="viewMode !== 'columns'" :content="t('peer.open')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
                <button
                  type="button"
                  class="bar-action"
                  data-test="open"
                  :aria-label="t('peer.open')"
                  :disabled="busy"
                  @click="runSelectionAction(([entry]) => browse(entry.id))"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" /></svg>
                </button>
              </el-tooltip>
              <el-tooltip :content="t('peer.download')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="bar-action" data-test="download" :aria-label="t('peer.download')" :disabled="busy || fileCapabilities?.download === false || fileCapabilities?.downloadDirectory === false" @click="runSelectionAction(([entry]) => downloadDirectory(entry))">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v10" /><path d="m8 10 4 4 4-4" /><path d="M4 16v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" /></svg>
                </button>
              </el-tooltip>
            </template>
            <template v-else-if="selectedEntries.length === 1">
              <el-tooltip v-if="canPreview(selectedEntry) && fileCapabilities?.preview !== false" :content="t('peer.preview')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="bar-action" data-test="preview" :aria-label="t('peer.preview')" :disabled="busy" @click="runSelectionAction(([entry]) => previewEntry(entry))">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" /><circle cx="12" cy="12" r="2.8" /></svg>
                </button>
              </el-tooltip>
              <el-tooltip v-if="fileCapabilities?.onlinePreview && selectedEntry.onlinePreview" :content="t('peer.onlinePreview')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="bar-action" data-test="online-preview" :aria-label="t('peer.onlinePreview')" :disabled="busy" @click="runSelectionAction(([entry]) => onlinePreviewEntry(entry))">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8" /><path d="M4 12h16" /><path d="M12 4c2.4 2.3 2.4 13.7 0 16M12 4c-2.4 2.3-2.4 13.7 0 16" /></svg>
                </button>
              </el-tooltip>
              <el-tooltip :content="t('peer.download')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
                <button type="button" class="bar-action" data-test="download" :aria-label="t('peer.download')" :disabled="busy || fileCapabilities?.download === false" @click="runSelectionAction(([entry]) => downloadEntry(entry))">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v10" /><path d="m8 10 4 4 4-4" /><path d="M4 16v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" /></svg>
                </button>
              </el-tooltip>
              <el-tooltip
                v-if="!selectedEntry.protected && !peerListing?.protected && (fileCapabilities?.externalEdit ?? !adapter) && (isReadableText(selectedEntry) || /\.(docx?|xlsx?|pptx?|odt|ods|odp|rtf|pages|numbers|key)$/i.test(selectedEntry.name))"
                :content="t('peer.edit')"
                placement="top"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button
                  type="button"
                  class="bar-action"
                  data-test="edit"
                  :aria-label="t('peer.edit')"
                  :disabled="busy"
                  @click="runSelectionAction(([entry]) => editEntry(entry))"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 20h4L19.3 8.7a2.2 2.2 0 0 0-3.1-3.1L5 16.9V20Z" /><path d="m14.5 7.5 2 2" /></svg>
                </button>
              </el-tooltip>
            </template>
            <el-tooltip v-else :content="t('peer.download')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
              <button type="button" class="bar-action" data-test="download" :aria-label="t('peer.download')" :disabled="busy || fileCapabilities?.download === false || (selectedEntries.some(entry => entry.isDir) && fileCapabilities?.downloadDirectory === false)" @click="runSelectionAction(downloadSelected)">
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v10" /><path d="m8 10 4 4 4-4" /><path d="M4 16v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" /></svg>
              </button>
            </el-tooltip>
            <el-tooltip v-if="adapter && supportsMove" :content="t('drive.move')" placement="top" :show-after="200" :enterable="false"><button type="button" class="bar-action" data-test="move" :aria-label="t('drive.move')" :disabled="busy || selectedEntries.some(entry => entry.protected)" @click="showMove"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h7l2 2h9v11H3ZM12 14h6m-3-3 3 3-3 3"/></svg></button></el-tooltip>
            <el-tooltip
              v-if="canDeleteEntries && selectedEntries.every(entry => !entry.protected && entry.id !== rootPath) && !peerListing?.protected"
              :content="t('common.delete')"
              placement="top"
              :show-after="200"
              :hide-after="0"
              :enterable="false"
            >
              <button
                type="button"
                class="bar-action bar-action--danger"
                data-test="delete-file"
                :aria-label="t('common.delete')"
                :disabled="busy"
                @click="runSelectionAction(entries => entries.length === 1 ? deleteEntry(entries[0]) : deleteSelected(entries))"
              >
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16" /><path d="M9 7V5h6v2" /><path d="M6 7l1 12h10l1-12" /></svg>
              </button>
            </el-tooltip>
            <el-tooltip :content="t('peer.clearSelection')" placement="top" :show-after="200" :hide-after="0" :enterable="false">
              <button type="button" class="bar-action" data-test="clear-selection" :aria-label="t('peer.clearSelection')" :disabled="busy" @click="clearSelection">
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" /></svg>
              </button>
            </el-tooltip>
          </div>
        </div>

        <section v-if="peerEdit" class="edit-panel">
          <div>
            <strong>{{ t("peer.externalEdit", { path: peerEdit.name }) }}</strong>
            <p class="hint mono">{{ peerEdit.localPath }}</p>
          </div>
          <div class="actions">
            <el-button type="primary" :disabled="busy" @click="commitPeerEdit">
              {{ t("peer.uploadChanges") }}
            </el-button>
            <el-button plain :disabled="busy" @click="discardPeerEdit">
              {{ t("peer.discard") }}
            </el-button>
          </div>
        </section>

        <p v-if="peerListing?.truncated" class="hint">
          {{ t("peer.truncated") }}
        </p>
      </main>
    </div>

    <TransferQueueDialog :open="downloadDialogOpen" :items="downloadQueue" :running="downloadRunning" :canceling="downloadCanceling"
      :summary="downloadSummary" :queue-state="downloadState" :next-offset="downloadPage.nextOffset" :page-loading="downloadPageLoading"
      v-model:apply-remaining="downloadApplyRemaining" :resolving="downloadResolving" @resolve="resolveDownloadConflict" @page="loadDownloadPage"
      @cancel="cancelDownloadQueue" @close="closeDownloadQueue" @reveal="revealDownloadRoot" />

    <UploadBatchDialog :open="uploadDialogOpen" :summary="uploadSummary" :page="uploadPage" :running="uploadRunning"
      :batch-error="uploadBatchError" :page-notice="uploadPageNotice"
      v-model:apply-remaining="uploadApplyRemaining" :resolving="uploadResolving" :overwrite-allowed="uploadOverwriteAllowed"
      @close="closeUpload" @cancel="cancelUpload" @resolve="resolveUploadConflict" />

    <el-dialog v-model="moveOpen" :title="t('drive.move')" width="480px">
      <DirectoryTreePicker :open="moveOpen" v-model="moveDestination" :load-children="listMoveFolders" @loading="moveLoading = $event" @error="app.handleFailure" />
      <template #footer><el-button @click="moveOpen = false">{{ t('common.cancel') }}</el-button><el-button type="primary" :disabled="moveLoading" @click="moveSelected">{{ t('drive.move') }}</el-button></template>
    </el-dialog>
    <FilePreviewDialog
      :open="previewOpen"
      :target="previewTarget"
      :preview="activePreview"
      :limit="previewLimit"
      :busy="busy"
      @close="closePreview"
      @download="downloadEntry(previewTarget)"
    />
  </div>
</template>

<style scoped>
.action-spinner { transform-box: view-box; transform-origin: 50% 50%; animation: file-spin 0.9s linear infinite; }
@keyframes file-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) { .action-spinner { animation: none; } }
.peer-browser--embedded { flex: 1; min-height: 0; }
.peer-browser--embedded > .browser-body { grid-template-columns: minmax(0, 1fr); }
.peer-browser--embedded .browser-main { padding: 0; }
.peer-browser {
  position: relative;
  flex: 1;
  min-height: 0;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.sidebar-identity {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 2px 8px 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--app-border);
}

.sidebar-identity strong,
.sidebar-identity span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.sidebar-identity strong {
  font-size: 13px;
  color: var(--app-text);
}

.sidebar-identity span {
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 11px;
  color: var(--app-text-muted);
}

.view-switch {
  flex: none;
  display: flex;
  gap: 2px;
  padding: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
}

.view-switch__button {
  width: 26px;
  height: 26px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 0;
  border-radius: 999px;
  background: transparent;
  color: var(--app-text-muted);
  cursor: pointer;
  -webkit-app-region: no-drag;
}

.view-switch__button:hover {
  color: var(--app-text);
  background: var(--pill-bg);
}

.view-switch__button:focus-visible {
  outline: 2px solid var(--el-color-primary);
  outline-offset: 1px;
}

.view-switch__button:disabled {
  opacity: 0.4;
  cursor: default;
}

.view-switch__button:disabled:hover {
  color: var(--app-text-muted);
  background: transparent;
}

.view-switch__button.is-active {
  color: var(--el-color-primary);
  background: var(--app-surface);
  box-shadow: 0 1px 4px rgba(22, 32, 46, 0.1);
}

.view-switch__button svg {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}

/* 快捷键入口是说明类图标，描边略细一点，避免和上传操作抢视觉权重。 */
.shortcuts-trigger svg {
  stroke-width: 1.4;
}

.shortcuts-panel {
  /* 定位由内联 style 提供（fixed + top/right），这里只负责外观与层叠。 */
  z-index: 1500;
  display: grid;
  gap: 8px;
  width: 300px;
  max-height: min(60vh, 420px);
  overflow: auto;
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 12px;
  background: var(--app-surface);
  box-shadow: 0 12px 28px rgba(22, 32, 46, 0.16);
  color: var(--app-text);
}

.shortcuts-panel__title {
  margin: 0;
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text-muted);
}

.shortcuts-panel__list {
  display: grid;
  gap: 6px;
  margin: 0;
}

.shortcuts-panel__row {
  display: grid;
  grid-template-columns: minmax(104px, auto) 1fr;
  gap: 10px;
  align-items: baseline;
}

.shortcuts-panel__keys {
  margin: 0;
}

.shortcuts-panel__keys kbd {
  padding: 2px 6px;
  border: 1px solid var(--app-border);
  border-radius: 6px;
  background: var(--app-surface-2);
  color: var(--app-text);
  font-family: inherit;
  font-size: 11px;
  white-space: nowrap;
}

.shortcuts-panel__desc {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--app-text-muted);
}

.shortcuts-panel__note {
  margin: 0;
  padding-top: 6px;
  border-top: 1px solid color-mix(in oklab, var(--app-border) 55%, transparent);
  font-size: 11px;
  line-height: 1.5;
  color: var(--app-text-muted);
}

.browser-body {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 180px minmax(0, 1fr);
  overflow: hidden;
  background: var(--browser-body-bg);
}

.browser-sidebar {
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 14px 10px;
  overflow-y: auto;
  background: color-mix(in oklab, var(--app-surface-2) 72%, transparent);
  border-right: 1px solid var(--app-border);
}

.sidebar-label {
  margin: 0 8px 6px;
  font-size: 11px;
  font-weight: 600;
  color: var(--app-text-muted);
}

.share-link {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 9px;
  border: 0;
  border-radius: 8px;
  background: transparent;
  color: var(--app-text-secondary);
  font: inherit;
  font-size: 12.5px;
  text-align: left;
  cursor: pointer;
}

.share-link :deep(.file-glyph) {
  width: 18px;
  height: 18px;
}

.share-link span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.share-link:hover {
  background: var(--app-surface-3);
}

.share-link.is-active {
  color: var(--app-text);
  background: var(--app-surface-2);
  box-shadow: 0 1px 5px rgba(22, 32, 46, 0.07);
}

.sidebar-empty {
  margin: 6px 8px;
  font-size: 12px;
  line-height: 1.5;
  color: var(--app-text-muted);
}

.browser-main {
  position: relative;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  padding: 14px 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 浮动操作条会盖住列表末尾，选中时把主区底部垫高到条体高度以上。 */
.browser-main--selection {
  padding-bottom: calc(14px + var(--floating-bar-height));
}

.peer-browser--embedded .browser-main.browser-main--selection {
  padding-bottom: calc(10px + var(--floating-bar-height));
}

.pathbar {
  display: flex;
  align-items: center;
  gap: 12px;
  border-bottom: 1px solid var(--app-border);
  padding-bottom: 12px;
}

.breadcrumbs {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 2px;
  overflow-x: auto;
  overflow-y: hidden;
  white-space: nowrap;
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.breadcrumbs::-webkit-scrollbar {
  display: none;
}

.breadcrumb {
  flex: none;
  border: 0;
  border-radius: 6px;
  padding: 4px 6px;
  background: transparent;
  color: var(--app-text-muted);
  font: inherit;
  font-size: 12.5px;
  cursor: pointer;
}

.breadcrumb:hover {
  color: var(--app-text);
  background: var(--app-surface-2);
}

.breadcrumb.is-current {
  color: var(--app-text);
  font-weight: 600;
}

.breadcrumb-sep {
  flex: none;
  color: var(--app-text-muted);
  font-size: 14px;
}

.pathbar-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 0;
  -webkit-app-region: no-drag;
}


/* Keep the pathbar and selection bar fixed; each view owns its scrolling region. */
.browser-main > .file-view {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
}

/* The region is only a keyboard-nav root; children carry their own focus ring. */
.browser-main > .file-view:focus {
  outline: none;
}

/* Drag selection owns the pointer in this region; text stays selectable where
   editing or reading it is the point (code preview, inputs). */
.browser-main > .file-view {
  position: relative;
  user-select: none;
  -webkit-user-select: none;
}

.browser-main > .file-view pre,
.browser-main > .file-view input,
.browser-main > .file-view textarea {
  user-select: text;
  -webkit-user-select: text;
}

.marquee {
  position: absolute;
  z-index: 5;
  pointer-events: none;
  border: 1px solid var(--el-color-primary);
  border-radius: 4px;
  background: color-mix(in oklab, var(--el-color-primary) 14%, transparent);
}

.gallery-image {
  max-width: 100%;
  max-height: 100%;
  width: auto;
  height: auto;
  object-fit: contain;
  border-radius: 8px;
  box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16);
}

.gallery-audio {
  width: min(440px, 100%);
}

.gallery-video {
  max-width: 100%;
  max-height: 100%;
  width: auto;
  height: auto;
  object-fit: contain;
  border-radius: 8px;
  background: #000;
  box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16);
}

.gallery-text {
  margin: 0;
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  min-height: 0;
  max-height: none;
  overflow: auto;
  padding: 16px;
  border-radius: 8px;
  background: var(--app-surface);
  border: 1px solid var(--app-border);
  font-size: 12px;
  line-height: 1.55;
  white-space: pre-wrap;
  word-break: break-word;
}

.gallery-placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 7px;
  color: var(--app-text-muted);
  text-align: center;
}

.gallery-placeholder strong {
  color: var(--app-text);
  font-size: 14px;
}

.gallery-placeholder span {
  font-size: 12px;
}

.file-empty {
  margin: 22px 0;
  text-align: center;
  font-size: 13px;
  color: var(--app-text-muted);
}

.file-view--placeholder {
  overflow: hidden;
}

/* 阿里盘式浮动操作条：底部居中、反色胶囊，明暗外观下背景与正文互换。 */
.selection-bar {
  position: absolute;
  left: 50%;
  bottom: 14px;
  transform: translateX(-50%);
  z-index: 7;
  box-sizing: border-box;
  min-height: 54px;
  max-width: calc(100% - 48px);
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 16px;
  padding: 8px 14px;
  border: 1px solid var(--floating-bar-border);
  border-radius: 999px;
  background: var(--floating-bar-bg);
  color: var(--floating-bar-fg);
  box-shadow: var(--floating-bar-shadow);
  backdrop-filter: blur(12px);
}

.bar-action {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex: none;
  width: 36px;
  height: 36px;
  padding: 0;
  border: 0;
  border-radius: 50%;
  background: transparent;
  color: inherit;
  cursor: pointer;
}

.bar-action:hover:not(:disabled) {
  background: var(--floating-bar-hover);
}

.bar-action:disabled {
  opacity: 0.45;
  cursor: default;
}

.bar-action svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.bar-action--danger {
  color: var(--floating-bar-danger);
}

.selection-actions {
  flex: none;
  display: flex;
  gap: 6px;
}

.edit-panel {
  display: flex;
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 12px;
  background: var(--app-surface-2);
}

.edit-panel p {
  margin: 4px 0 0;
}

@media (max-width: 760px) {
  .browser-body {
    grid-template-columns: 1fr;
  }

  .browser-sidebar {
    flex-direction: row;
    align-items: center;
    overflow-x: auto;
    border-right: 0;
    border-bottom: 1px solid var(--app-border);
  }

  .sidebar-identity {
    flex: none;
    margin: 0 10px 0 4px;
    padding: 0 12px 0 0;
    border-bottom: 0;
    border-right: 1px solid var(--app-border);
  }

  .sidebar-label,
  .sidebar-empty {
    display: none;
  }

  .share-link {
    width: auto;
    flex: none;
  }

  .edit-panel {
    align-items: stretch;
    flex-direction: column;
  }

  .selection-actions {
    flex-wrap: wrap;
  }

  .pathbar {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
