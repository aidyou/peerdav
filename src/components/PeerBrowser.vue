<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import UploadBatchDialog from "./UploadBatchDialog.vue";
import TransferQueueDialog from "./TransferQueueDialog.vue";
import { useUploadBatch } from "../composables/useUploadBatch";
import { peerUploadAdapter } from "../lib/uploadBatchAdapters";
import { useDownloadQueue } from "../composables/useDownloadQueue";

import FileGlyph from "./FileGlyph.vue";
import { selectionForClick } from "../lib/fileManagerAdapters";
import FileManagerView from "./FileManagerView.vue";
import FilePreviewDialog from "./FilePreviewDialog.vue";
import { useAppContext } from "../lib/appContext";
import { confirmAction } from "../lib/confirm";
import { canPreview, fileKind, fileKindKey, isReadableText } from "../lib/fileKinds";
import { t } from "../lib/i18n";
import { formatBytes } from "../lib/format";
import { generalSettings } from "../lib/general";
import { normalizeAppError } from "../lib/invoke";
import {
  MAX_PREVIEW_BYTES,
  previewFormat as detectPreviewFormat,
} from "../lib/textPreview";
import { useAction } from "../composables/useAction";

const props = defineProps({
  peer: { type: Object, required: true },
});

const app = useAppContext();
const { busy, run } = useAction();

const VIEW_STORAGE_KEY = "peerdav.peerView";
const VIEW_MODES = ["icons", "list", "columns", "gallery"];

const peerShares = ref([]);
const activeShareId = ref("");
const peerPath = ref("/");
const peerListing = ref(null);
const peerPreview = ref(null);
const peerPreviewPath = ref("");
const previewOpen = ref(false);
const previewLimit = ref(null);
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
const canMutate = computed(() => Boolean(activeShare.value?.permissions?.canWrite) && !busy.value && !uploadRunning.value);
const {
  uploadDialogOpen, uploadSummary, uploadPage, uploadRunning,
  uploadBatchError, uploadPageNotice, uploadApplyRemaining, uploadResolving, uploadOverwriteAllowed,
  selectUpload, pollUpload, cancelUpload, closeUpload, resolveUploadConflict, cancelUploadSession,
} = useUploadBatch({
  adapter: peerUploadAdapter,
  captureScope: () => ({ peerId: props.peer.id, shareName: activeShare.value.name, shareId: activeShareId.value,
    remoteDir: peerPath.value, options: { respectGitignore: generalSettings.respectGitignore, patterns: generalSettings.ignorePatterns } }),
  scopeValid: (scope) => !browserDisposed && scope.peerId === props.peer.id && scope.shareId === activeShareId.value,
  canSelect: () => canMutate.value,
  onFailure: (cause) => app.handleFailure(cause),
  onCompleted: (summary, scope) => {
    app.setNotice("peer.uploadFinished", { count: summary.completedFiles });
    if (peerPath.value === scope.remoteDir) void loadPath(scope.remoteDir);
  },
});
const {
  downloadDialogOpen, downloadState, downloadQueue, downloadPage, downloadRunning, downloadCanceling,
  downloadResolving, downloadApplyRemaining, downloadPageLoading, downloadSummary, selectDownload, pollDownload,
  loadDownloadPage, cancelDownloadQueue, resolveDownloadConflict, closeDownloadQueue, revealDownloadRoot, disposeDownload,
} = useDownloadQueue({
  captureScope: () => ({ peerId: props.peer.id, shareId: activeShareId.value }),
  scopeValid: (scope) => !browserDisposed && scope.peerId === props.peer.id && scope.shareId === activeShareId.value,
  backend: (scope) => ({ backend: "peer", ...scope }),
  onFailure: (cause) => app.handleFailure(cause),
});
onMounted(() => { uploadPollTimer = setInterval(() => { void pollUpload(); void pollDownload(); }, 1500); });
watch([() => props.peer.id, activeShareId], () => { void cancelUploadSession(); void disposeDownload(); }, { flush: "sync" });

onBeforeUnmount(() => {
  browserDisposed = true;
  clearInterval(uploadPollTimer);
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
  Boolean(activeShare.value?.permissions?.canDelete),
);

const peerHost = computed(() => {
  try {
    return new URL(props.peer.url).hostname || props.peer.url;
  } catch {
    return props.peer.url;
  }
});

const breadcrumbs = computed(() => {
  const crumbs = [
    {
      name: activeShare.value?.name || t("peer.root"),
      path: "/",
    },
  ];
  let path = "";
  for (const segment of peerPath.value.split("/").filter(Boolean)) {
    path += `/${segment}`;
    crumbs.push({ name: segment, path });
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
  peerPreview.value && peerPreviewPath.value === selectedEntry.value?.path
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
let navigationToken = 0;

function applyListing(listing) {
  peerListing.value = listing;
  peerPath.value = listing.path;
  clearSelection();
  clearPreview();
  if (viewMode.value === "columns") {
    columnStack.value = [listing];
  }
}

async function fetchListing(path) {
  return app.invoke("list_peer_dir", {
    peerId: props.peer.id,
    shareId: activeShareId.value,
    path,
  });
}

async function rebuildColumnStack() {
  const current = peerListing.value;
  if (!current) return;
  const targetPath = peerPath.value;
  const listings = [];

  try {
    for (const crumb of breadcrumbs.value) {
      listings.push(
        crumb.path === targetPath ? current : await fetchListing(crumb.path),
      );
    }
  } catch (error) {
    if (viewMode.value !== "columns" || peerPath.value !== targetPath) return;
    app.handleFailure(error);
    columnStack.value = [current];
    return;
  }

  if (viewMode.value !== "columns" || peerPath.value !== targetPath) return;
  columnStack.value = listings;
}

// 进入目录时先更新地址栏并清空文件区放上 loading，让"已进入"立刻可见；
// 令牌保证用户中途再导航时，旧请求返回的内容不会覆盖新目录。
async function loadPath(path) {
  const token = ++navigationToken;
  peerPath.value = path;
  peerListing.value = null;
  loadFailed.value = false;
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
    app.handleFailure(error);
  } finally {
    if (token === navigationToken) navigating.value = false;
  }
}

async function loadPeer() {
  reset();
  await run(async () => {
    peerShares.value = await app.invoke("list_peer_shares", {
      peerId: props.peer.id,
    });
    if (peerShares.value.length) {
      activeShareId.value = peerShares.value[0].id;
      await loadPath("/");
    }
  });
}

async function openShare(shareId) {
  if (!props.peer.id) return;
  activeShareId.value = shareId;
  void loadPath("/");
}

// 导航不再被 busy 阻塞：用户可以在下载等操作进行中继续切换目录，
// 竞态由 loadPath 内的令牌保证。
async function browse(path) {
  if (!props.peer.id || !activeShareId.value) return;
  void loadPath(path);
}

async function refreshListing() {
  if (!activeShareId.value) return;
  void loadPath(peerPath.value);
}

function isSelected(entry) {
  return selectedEntries.value.some((item) => item.path === entry?.path);
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

const selectionFileCount = computed(
  () => selectedEntries.value.filter((entry) => !entry.isDir).length,
);
const selectionDirCount = computed(
  () => selectedEntries.value.length - selectionFileCount.value,
);
const selectionTitle = computed(() =>
  selectedEntries.value.length > 1
    ? t("peer.selectedCount", { count: selectedEntries.value.length })
    : selectedEntry.value?.name ?? "",
);
const selectionSubtitle = computed(() => {
  if (selectedEntries.value.length > 1) {
    return t("peer.selectedSummary", {
      files: selectionFileCount.value,
      dirs: selectionDirCount.value,
    });
  }
  if (!selectedEntry.value) return "";
  return entryKind(selectedEntry.value) +
    (selectedEntry.value.isDir ? "" : ` · ${formatSize(selectedEntry.value)}`);
});
const selectionGlyph = computed(() =>
  selectedEntries.value.length > 1
    ? { isDir: true, name: "", contentType: null }
    : selectedEntry.value,
);

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
  selectionAnchor.value = entry.path;
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
    anchor: selectionAnchor.value, entry, identityField: "path", additive, ranged });
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
  setSelection(listing.filter((entry) => hits.includes(entry.path)));
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
    void browse(entry.path);
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
  const current = entries.findIndex((entry) => entry.path === selectedEntry.value?.path);
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
  void browse(crumbs[crumbs.length - 2].path);
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

async function loadColumn(entry, index) {
  if (!entry.isDir) {
    selectSingle(entry);
    return;
  }
  if (busy.value) return;
  loadingColumn.value = index;
  try {
    await run(async () => {
      const listing = await fetchListing(entry.path);
      peerListing.value = listing;
      peerPath.value = listing.path;
      clearPreview();
      setSelection([entry]);
      columnStack.value = [...columnStack.value.slice(0, index + 1), listing];
    });
  } finally {
    // 失败时 run 会吞掉异常并走 handleFailure，这里保证指示器一定会撤掉。
    if (loadingColumn.value === index) loadingColumn.value = -1;
  }
}

function downloadEntry(entry) {
  if (!entry || entry.isDir || busy.value) return;
  return selectDownload({ mode: "single", path: entry.path });
}
function downloadDirectory(entry) {
  if (!entry?.isDir || busy.value) return;
  return selectDownload({ mode: "folder", path: entry.path });
}
function downloadBatch(entries) {
  if (!entries.length || busy.value) return;
  return selectDownload({ mode: "files", paths: entries.map((entry) => entry.path) });
}

function runSelectionAction(action) {
  const entries = selectedEntries.value.slice();
  clearSelection();
  if (entries.length) return action(entries);
}

function downloadSelected(entries = selectedEntries.value) {
  if (!entries.length || busy.value) return;
  if (entries.length === 1) {
    const entry = entries[0];
    if (entry.isDir) void downloadDirectory(entry);
    else void downloadEntry(entry);
    return;
  }
  void downloadBatch(entries);
}

function chooseUploadFiles() { return selectUpload("files"); }
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
  if (!entry || entry.isDir || busy.value || !canPreview(entry)) return;
  clearPreview();
  setSelection([entry]);

  if (Number(entry.size) > MAX_PREVIEW_BYTES) {
    peerPreviewPath.value = entry.path;
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
      const preview = await app.invoke("preview_peer_file", {
        peerId: props.peer.id,
        shareId: activeShareId.value,
        remotePath: entry.path,
      });
      peerPreview.value = preview;
      peerPreviewPath.value = entry.path;
      previewOpen.value = previewUsesDialog(entry, preview);
    } catch (error) {
      if (
        error?.code === "PEER_PREVIEW_TOO_LARGE" ||
        error?.code === "PEER_PREVIEW_TOO_MANY_LINES"
      ) {
        peerPreviewPath.value = entry.path;
        previewLimit.value = { code: error.code, ...(error.context ?? {}) };
        previewOpen.value = true;
        return;
      }
      throw error;
    }
  });
}

async function editEntry(entry) {
  if (!entry || entry.isDir || busy.value) return;
  await run(async () => {
    peerEdit.value = await app.invoke("prepare_peer_external_edit", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
    setPeerNotice("peer.editOpened");
  });
}

async function deleteEntry(entry) {
  if (!entry || busy.value || !canDeleteEntries.value) return;
  await run(async () => {
    const confirmed = await confirmAction(
      t(entry.isDir ? "peer.deleteFolderConfirm" : "peer.deleteFileConfirm", {
        name: entry.name,
      }),
      t("common.delete"),
      { danger: true, confirmText: t("common.delete") },
    );
    if (!confirmed) return;

    if (peerEdit.value?.remotePath === entry.path) {
      await app.invoke("discard_peer_external_edit", {
        editId: peerEdit.value.editId,
      });
      peerEdit.value = null;
    }
    await app.invoke("delete_peer_entry", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
    await loadPath(peerPath.value);
    app.setNotice("peer.deleted", { name: entry.name });
  });
}

async function commitPeerEdit() {
  if (!peerEdit.value || busy.value) return;
  await run(async () => {
    let result;
    try {
      result = await app.invoke("commit_peer_external_edit", {
        editId: peerEdit.value.editId,
        conflict: "fail",
      });
    } catch (error) {
      const normalized = normalizeAppError(error);
      if (normalized.code !== "TRANSFER_CONFLICT") throw error;
      const overwrite = await confirmAction(
        t("peer.editConflict"),
        t("peer.uploadChanges"),
        {
          confirmText: t("peer.overwrite"),
          cancelText: t("peer.skip"),
          danger: true,
        },
      );
      if (!overwrite) {
        setPeerNotice("peer.editSkipped");
        return;
      }
      result = await app.invoke("commit_peer_external_edit", {
        editId: peerEdit.value.editId,
        conflict: "overwrite",
      });
    }
    if (result.transferred) {
      setPeerNotice("peer.editUploaded", { path: result.remotePath });
    } else {
      setPeerNotice("peer.editNoChange");
    }
    peerEdit.value = null;
    await loadPath(peerPath.value);
  });
}

async function discardPeerEdit() {
  if (!peerEdit.value || busy.value) return;
  await run(async () => {
    await app.invoke("discard_peer_external_edit", {
      editId: peerEdit.value.editId,
    });
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
  if (!seconds) return "—";
  try {
    return new Intl.DateTimeFormat(document.documentElement.lang || undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(Number(seconds) * 1000));
  } catch {
    return "—";
  }
}

watch(() => props.peer.id, loadPeer, { immediate: true });
</script>

<template>
  <div class="peer-browser">
    <div class="browser-body">
      <aside class="browser-sidebar">
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

      <main class="browser-main">
        <div class="pathbar">
          <div
            ref="breadcrumbsEl"
            class="breadcrumbs"
            aria-label="Breadcrumb"
            @wheel="scrollBreadcrumbs"
          >
            <template v-for="(crumb, index) in breadcrumbs" :key="crumb.path">
              <span v-if="index" class="breadcrumb-sep">›</span>
              <button
                type="button"
                class="breadcrumb"
                :class="{ 'is-current': index === breadcrumbs.length - 1 }"
                @click="browse(crumb.path)"
              >
                {{ crumb.name }}
              </button>
            </template>
          </div>
        </div>

        <Teleport to="#titlebar-file-actions">
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
              <el-tooltip
                :content="t('peer.refresh')"
                placement="bottom"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button
                  type="button"
                  class="view-switch__button"
                  :aria-label="t('peer.refresh')"
                  :disabled="busy || !activeShareId"
                  @click="refreshListing"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M20 11a8 8 0 1 0-2.3 5.7" />
                    <path d="M20 5v6h-6" />
                  </svg>
                </button>
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
                  :disabled="!canMutate"
                  @click="chooseUploadFolder"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" />
                    <path d="M12 17v-5M9.5 14.5 12 12l2.5 2.5" />
                  </svg>
                </button>
              </el-tooltip>
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
          <FileManagerView :entries="peerListing.entries" :mode="viewMode" :selection="selectedEntries"
            :columns="visibleColumns" :column-loading="loadingColumn !== -1" :append-loading-column="loadingColumn !== -1"
            :partial="!!peerListing.truncated" time-column="modified" :entry-kind="entryKind" :format-size="formatSize" :format-modified="formatModified"
            @select="selectEntry" @activate="openEntry" @column-select="(entry, event, column, index) => loadColumn(entry, index)"
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
          </FileManagerView>
        </div>

        <!-- 加载中先清空旧内容并给出即时反馈；失败时提示可点击地址栏重试。 -->
        <div v-else class="file-view file-view--placeholder" :aria-busy="navigating">
          <FileManagerView v-if="navigating" :mode="viewMode" loading :entry-kind="entryKind" :format-size="formatSize" />
          <p v-else class="file-empty">{{ t("peer.loadFailed") }}</p>
        </div>

        <div
          v-if="selectedEntries.length"
          class="selection-bar"
          :class="{
            'selection-bar--reserved': !selectedEntries.length,
            'selection-bar--multi': selectedEntries.length > 1,
          }"
        >
          <div v-if="selectedEntries.length" class="selection-summary">
            <FileGlyph :entry="selectionGlyph" />
            <div>
              <strong>{{ selectionTitle }}</strong>
              <span>{{ selectionSubtitle }}</span>
            </div>
            <el-button
              v-if="selectedEntries.length > 1"
              class="selection-clear"
              link
              type="primary"
              :disabled="busy"
              @click="clearSelection"
            >
              {{ t("peer.clearSelection") }}
            </el-button>
          </div>
          <p v-else class="selection-bar__hint">{{ t("peer.selectionHint") }}</p>
          <div v-if="selectedEntries.length" class="selection-actions">
            <template v-if="selectedEntries.length === 1 && selectedEntry.isDir">
              <el-button
                v-if="viewMode !== 'columns'"
                type="primary"
                :disabled="busy"
                @click="runSelectionAction(([entry]) => browse(entry.path))"
              >
                {{ t("peer.open") }}
              </el-button>
              <el-button plain :disabled="busy" @click="runSelectionAction(([entry]) => downloadDirectory(entry))">
                {{ t("peer.download") }}
              </el-button>
            </template>
            <template v-else-if="selectedEntries.length === 1">
              <el-button plain :disabled="busy" @click="runSelectionAction(([entry]) => downloadEntry(entry))">
                {{ t("peer.download") }}
              </el-button>
              <el-button
                v-if="isReadableText(selectedEntry)"
                plain
                :disabled="busy"
                @click="runSelectionAction(([entry]) => editEntry(entry))"
              >
                {{ t("peer.edit") }}
              </el-button>
            </template>
            <el-button v-else type="primary" plain :disabled="busy" @click="runSelectionAction(downloadSelected)">
              {{ t("peer.download") }}
            </el-button>
            <el-button
              v-if="canDeleteEntries && selectedEntries.length === 1"
              type="danger"
              plain
              :disabled="busy"
              @click="runSelectionAction(([entry]) => deleteEntry(entry))"
            >
              {{ t("common.delete") }}
            </el-button>
          </div>
        </div>

        <section v-if="peerEdit" class="edit-panel">
          <div>
            <strong>{{ t("peer.externalEdit", { path: peerEdit.remotePath }) }}</strong>
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
  background: var(--app-surface);
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
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  padding: 14px 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
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

.selection-bar {
  flex: none;
  box-sizing: border-box;
  min-height: 54px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-2);
}

.selection-bar--reserved {
  border-style: dashed;
  background: transparent;
}

.selection-bar--multi {
  align-items: center;
}

.selection-clear {
  flex: none;
  margin-left: auto;
  padding: 0;
  height: auto;
  min-height: 0;
}

.selection-bar__hint {
  margin: 0;
  font-size: 12px;
  color: var(--app-text-muted);
}

.selection-summary {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 8px;
}

.selection-summary div {
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.selection-summary strong,
.selection-summary span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.selection-summary strong {
  font-size: 12.5px;
  color: var(--app-text);
}

.selection-summary span {
  font-size: 11px;
  color: var(--app-text-muted);
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

  .selection-bar,
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
