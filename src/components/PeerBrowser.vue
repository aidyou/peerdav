<script setup>
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

import FileGlyph from "./FileGlyph.vue";
import { useAppContext } from "../lib/appContext";
import { confirmAction } from "../lib/confirm";
import { canPreview, fileKind, fileKindKey, isReadableText } from "../lib/fileKinds";
import { t } from "../lib/i18n";
import { generalSettings } from "../lib/general";
import { normalizeAppError, toUserMessage } from "../lib/invoke";
import { statusTagTone } from "../lib/tagTone";
import {
  MAX_PREVIEW_BYTES,
  MAX_PREVIEW_LINES,
  parseDelimited,
  previewFormat as detectPreviewFormat,
  tableDelimiter,
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
const previewDialogEl = ref(null);
const previewRenderTools = shallowRef(null);
let previewRenderToolsPromise = null;
const peerEdit = ref(null);
const peerNoticeState = ref(null);
const viewMode = ref(readViewMode());
const columnStack = ref([]);
const columnViewEl = ref(null);
const fileViewEl = ref(null);
const selectedEntry = ref(null);
const selectedEntries = ref([]);
const selectionAnchor = ref(null);
const marquee = ref(null);
const batchConflict = ref("fail");
let batchConflictResolved = false;
let marqueeStart = null;
let suppressClick = false;
const downloadOpen = ref(false);
const downloadName = ref("");
const downloadTransferId = ref("");
const downloadDialogOpen = ref(false);
const downloadDirectoryName = ref("");
const downloadRoot = ref("");
const downloadQueue = ref([]);
const downloadRunning = ref(false);
const downloadCanceled = ref(false);
const savedOpen = ref(false);
const savedPath = ref("");
const uploadDialogOpen = ref(false);
const uploadQueue = ref([]);
const uploadRunning = ref(false);
const uploadCanceled = ref(false);
const uploadTransferId = ref("");
const uploadClock = ref(Date.now());
let uploadClockTimer;
let browserDisposed = false;
watch(uploadRunning, (running) => {
  clearInterval(uploadClockTimer);
  if (running) {
    uploadClockTimer = setInterval(() => { uploadClock.value = Date.now(); }, 500);
  }
});
const currentUpload = computed(() =>
  uploadQueue.value.find((item) => item.status === "uploading"),
);
const uploadTransfer = computed(
  () => app.transfers?.[uploadTransferId.value] ?? null,
);
const uploadPhase = computed(() => uploadTransfer.value?.phase ?? "hashing");
const uploadFilePercent = computed(() => {
  const progress = uploadTransfer.value;
  if (!progress?.total) return 0;
  return Math.min(100, Math.floor((progress.transferred / progress.total) * 100));
});
const uploadSpeed = computed(() => {
  const progress = uploadTransfer.value;
  if (
    uploadPhase.value !== "uploading" ||
    !progress?.updatedAt ||
    uploadClock.value - progress.updatedAt > 2000
  ) return 0;
  return progress.bytesPerSecond || 0;
});

onBeforeUnmount(() => {
  browserDisposed = true;
  uploadCanceled.value = true;
  clearInterval(uploadClockTimer);
  if (uploadTransferId.value && uploadRunning.value) {
    void app.invoke("cancel_peer_upload", {
      transferId: uploadTransferId.value,
    }).catch(app.handleFailure);
  }
  app.clearTransfer?.(uploadTransferId.value);
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

const peerNotice = computed(() => {
  if (!peerNoticeState.value) return "";
  return t(peerNoticeState.value.key, peerNoticeState.value.params);
});

const activeTransfer = computed(
  () => app.transfers?.[downloadTransferId.value] ?? null,
);

const downloadPercent = computed(() => {
  const transfer = activeTransfer.value;
  if (!transfer?.total) return 0;
  return Math.min(100, Math.round((transfer.transferred / transfer.total) * 100));
});

const downloadFinished = computed(
  () =>
    downloadQueue.value.filter(
      (item) => !["pending", "downloading"].includes(item.status),
    ).length,
);

const downloadTotalBytes = computed(() =>
  downloadQueue.value.reduce(
    (total, item) => total + (Number(item.size) || 0),
    0,
  ),
);

const downloadTransferredBytes = computed(() => {
  let transferred = downloadQueue.value
    .filter((item) => item.status === "done")
    .reduce(
      (total, item) => total + (Number(item.bytes ?? item.size) || 0),
      0,
    );
  const current = downloadQueue.value.find(
    (item) => item.status === "downloading",
  );
  if (current && activeTransfer.value) {
    transferred += Number(activeTransfer.value.transferred) || 0;
  }
  return transferred;
});

const downloadQueuePercent = computed(() => {
  if (!downloadQueue.value.length) return 0;
  if (downloadQueue.value.every((item) => item.status === "done")) return 100;
  if (downloadTotalBytes.value > 0) {
    return Math.min(
      100,
      Math.round(
        (downloadTransferredBytes.value / downloadTotalBytes.value) * 100,
      ),
    );
  }
  return Math.round(
    (downloadFinished.value / downloadQueue.value.length) * 100,
  );
});

const downloadSummary = computed(() =>
  t("peer.downloadSummary", {
    finished: downloadFinished.value,
    total: downloadQueue.value.length,
  }),
);

const uploadFinished = computed(
  () =>
    uploadQueue.value.filter(
      (item) => !["pending", "uploading"].includes(item.status),
    ).length,
);

const uploadPercent = computed(() => {
  if (!uploadQueue.value.length) return 0;
  const total = uploadQueue.value.reduce((sum, item) => sum + (Number(item.size) || 0), 0);
  const finished = uploadQueue.value.filter((item) => item.status === "done")
    .reduce((sum, item) => sum + (Number(item.size) || 0), 0);
  const current = currentUpload.value && ["uploading", "committing", "completed"].includes(uploadPhase.value)
    ? Number(uploadTransfer.value?.transferred) || 0 : 0;
  if (uploadQueue.value.every((item) => item.status === "done")) return 100;
  return total ? Math.min(99, Math.floor(((finished + current) / total) * 100)) : 0;
});

const uploadSummary = computed(() =>
  t("peer.uploadSummary", {
    finished: uploadFinished.value,
    total: uploadQueue.value.length,
  }),
);

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

watch(
  columnStack,
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
const previewFormatKind = computed(() =>
  detectPreviewFormat(previewTarget.value ?? {}),
);

const previewKind = computed(() => {
  if (!activePreview.value) {
    return previewLimit.value ? "unavailable" : "empty";
  }
  if (isImagePreview.value) return "image";
  if (isAudioPreview.value) return "audio";
  if (isVideoPreview.value) return "video";
  if (activePreview.value.encoding !== "utf8") return "binary";
  return previewFormatKind.value;
});

const codePreview = computed(() =>
  previewKind.value === "code" && previewRenderTools.value
    ? previewRenderTools.value.highlightCode(
        activePreview.value?.data,
        previewTarget.value,
      )
    : null,
);

const codeLineCount = computed(() =>
  previewKind.value === "code"
    ? Math.max(1, String(activePreview.value?.data ?? "").split("\n").length)
    : 0,
);

const markdownPreviewHtml = computed(() =>
  previewKind.value === "markdown" && previewRenderTools.value
    ? previewRenderTools.value.renderMarkdown(activePreview.value?.data)
    : "",
);

const tablePreview = computed(() =>
  previewKind.value === "table"
    ? parseDelimited(
        activePreview.value?.data,
        tableDelimiter(previewTarget.value),
      )
    : null,
);

const previewMeta = computed(() => {
  if (activePreview.value) {
    return `${activePreview.value.mime} · ${formatBytes(activePreview.value.size)}`;
  }
  if (previewTarget.value) return formatBytes(previewTarget.value.size);
  return "";
});

const previewLimitText = computed(() => {
  if (!previewLimit.value) return "";
  if (previewLimit.value.code === "PEER_PREVIEW_TOO_MANY_LINES") {
    return t("peer.previewTooManyLines", {
      lines: previewLimit.value.lines ?? "—",
      limit: previewLimit.value.limit ?? MAX_PREVIEW_LINES,
    });
  }
  if (previewLimit.value.code === "PEER_PREVIEW_TOO_LARGE") {
    return t("peer.previewTooLarge", {
      size: formatBytes(previewLimit.value.size ?? 0),
      limit: formatBytes(previewLimit.value.limit ?? MAX_PREVIEW_BYTES),
    });
  }
  return t("peer.previewUnavailable");
});

watch(previewOpen, async (open) => {
  if (!open) return;
  await nextTick();
  previewDialogEl.value?.focus();
});

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
  peerNoticeState.value = { key, params };
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
  peerNoticeState.value = null;
  columnStack.value = [];
  clearSelection();
  // 切换对端时作废仍在飞行的目录请求，避免旧结果落到新对端上。
  navigationToken += 1;
  navigating.value = false;
  loadFailed.value = false;
  downloadOpen.value = false;
  downloadTransferId.value = "";
  downloadDialogOpen.value = false;
  downloadDirectoryName.value = "";
  downloadRoot.value = "";
  downloadQueue.value = [];
  downloadRunning.value = false;
  downloadCanceled.value = false;
  savedOpen.value = false;
  savedPath.value = "";
  uploadDialogOpen.value = false;
  uploadQueue.value = [];
  uploadRunning.value = false;
  uploadCanceled.value = false;
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

function selectEntry(entry, event) {
  if (suppressClick) return;
  const listing = peerListing.value?.entries ?? [];
  if (event?.shiftKey && selectionAnchor.value) {
    const from = listing.findIndex((item) => item.path === selectionAnchor.value);
    const to = listing.findIndex((item) => item.path === entry.path);
    if (from !== -1 && to !== -1) {
      const [start, end] = from < to ? [from, to] : [to, from];
      setSelection(listing.slice(start, end + 1));
      return;
    }
  }
  if (event?.ctrlKey || event?.metaKey) {
    const added = !isSelected(entry);
    const next = added
      ? [...selectedEntries.value, entry]
      : selectedEntries.value.filter((item) => item.path !== entry.path);
    setSelection(listing.filter((item) => next.some((entry2) => entry2.path === item.path)));
    if (added) selectionAnchor.value = entry.path;
    return;
  }
  selectionAnchor.value = entry.path;
  setSelection([entry]);
  if (viewMode.value === "gallery" && !entry.isDir && canPreview(entry)) {
    void previewEntry(entry);
  }
}

// 任意位置按下并拖动都能框选（包括从文件上起拉），轻点空白处清空选择。
// preventDefault 关闭原生拖动：避免文字选择和"从下往上拉时区域自动向下滚动"。
function onFileViewMousedown(event) {
  if (event.button !== 0 || viewMode.value === "columns") return;
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
  selectEntry(entries[index]);
  selectionAnchor.value = entries[index].path;
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
    selectEntry(entry);
    return;
  }
  if (busy.value) return;
  await run(async () => {
    const listing = await fetchListing(entry.path);
    peerListing.value = listing;
    peerPath.value = listing.path;
    clearPreview();
    setSelection([entry]);
    columnStack.value = [...columnStack.value.slice(0, index + 1), listing];
  });
}

function newTransferId() {
  return globalThis.crypto?.randomUUID?.() ?? `transfer-${Date.now()}-${Math.random()}`;
}

function baseName(path) {
  return String(path).split(/[\\/]/).pop() || path;
}

function remoteDirFor(relativePath) {
  const parts = String(relativePath).split("/");
  parts.pop();
  const relativeDir = parts.join("/");
  if (!relativeDir) return peerPath.value;
  const base = peerPath.value === "/" ? "" : peerPath.value.replace(/\/+$/, "");
  return `${base}/${relativeDir}`;
}

async function downloadEntry(entry) {
  if (!entry || entry.isDir || busy.value) return;
  await run(async () => {
    const target = await saveDialog({
      title: t("peer.saveAs"),
      defaultPath: entry.name,
    });
    if (!target) return;
    // 下载已开始，选中状态不应残留在已下载的条目上。
    clearSelection();

    const transferId = newTransferId();
    downloadTransferId.value = transferId;
    downloadName.value = entry.name;
    downloadOpen.value = true;
    app.clearTransfer(transferId);

    try {
      const result = await app.invoke("download_peer_file_to", {
        peerId: props.peer.id,
        shareId: activeShareId.value,
        remotePath: entry.path,
        localPath: target,
        transferId,
      });
      savedPath.value = result.localPath || target;
      savedOpen.value = true;
    } finally {
      app.clearTransfer(transferId);
      downloadTransferId.value = "";
      downloadOpen.value = false;
    }
  });
}

function markRemainingDownloadCanceled() {
  for (const item of downloadQueue.value) {
    if (item.status === "pending") {
      item.status = "canceled";
    }
  }
}

function downloadStatusText(item) {
  const key = {
    pending: "peer.downloadStatusPending",
    downloading: "peer.downloadStatusDownloading",
    done: "peer.downloadStatusDone",
    canceled: "peer.downloadStatusCanceled",
    error: "peer.downloadStatusError",
  }[item.status];
  return key ? t(key) : "";
}

async function processDownloadQueue() {
  let completed = 0;
  for (const item of downloadQueue.value) {
    if (downloadCanceled.value) {
      if (item.status === "pending") item.status = "canceled";
      continue;
    }
    if (item.status !== "pending") continue;

    item.status = "downloading";
    const transferId = newTransferId();
    downloadTransferId.value = transferId;
    app.clearTransfer(transferId);
    try {
      const result = item.localDir
        ? await app.invoke("download_peer_file", {
            peerId: props.peer.id,
            shareId: activeShareId.value,
            remotePath: item.remotePath,
            localDir: item.localDir,
            conflict: batchConflict.value,
          })
        : await app.invoke("download_peer_file_to", {
            peerId: props.peer.id,
            shareId: activeShareId.value,
            remotePath: item.remotePath,
            localPath: item.localPath,
            transferId,
          });
      item.status = "done";
      item.bytes = result.bytes;
      completed += 1;
    } catch (error) {
      const conflict = item.localDir && normalizeAppError(error).code === "TRANSFER_CONFLICT";
      if (conflict) {
        const policy = await ensureBatchConflictPolicy(baseName(item.relativePath));
        if (policy) {
          try {
            const result = await app.invoke("download_peer_file", {
              peerId: props.peer.id,
              shareId: activeShareId.value,
              remotePath: item.remotePath,
              localDir: item.localDir,
              conflict: policy,
            });
            item.status = "done";
            item.bytes = result.bytes;
            completed += 1;
            continue;
          } catch (retryError) {
            item.status = "error";
            item.error = toUserMessage(retryError);
            markRemainingDownloadCanceled();
            app.handleFailure(retryError);
            break;
          }
        }
        item.status = "canceled";
        markRemainingDownloadCanceled();
        break;
      }
      item.status = "error";
      item.error = toUserMessage(error);
      markRemainingDownloadCanceled();
      app.handleFailure(error);
      break;
    } finally {
      app.clearTransfer(transferId);
      if (downloadTransferId.value === transferId) {
        downloadTransferId.value = "";
      }
    }
  }
  downloadRunning.value = false;
  if (!downloadCanceled.value && completed > 0) {
    app.setNotice("peer.downloadFinished", { count: completed });
  }
}

function cancelDownloadQueue() {
  if (!downloadRunning.value || downloadCanceled.value) return;
  downloadCanceled.value = true;
  markRemainingDownloadCanceled();
}

async function downloadDirectory(entry) {
  if (!entry?.isDir || busy.value) return;
  await run(async () => {
    const selected = await openDialog({
      title: t("peer.pickDownloadFolder"),
      multiple: false,
      directory: true,
    });
    const localParent = typeof selected === "string" ? selected : "";
    if (!localParent) return;
    // 下载已开始，选中状态不应残留在已下载的条目上。
    clearSelection();

    const request = (conflict) =>
      app.invoke("prepare_peer_directory_download", {
        peerId: props.peer.id,
        shareId: activeShareId.value,
        remotePath: entry.path,
        localParent,
        ignoreOptions: { respectGitignore: generalSettings.respectGitignore, patterns: generalSettings.ignorePatterns },
        conflict,
      });

    let plan;
    try {
      plan = await request("fail");
    } catch (error) {
      if (normalizeAppError(error).code !== "TRANSFER_CONFLICT") throw error;
      const overwrite = await confirmAction(
        t("peer.overwriteDirectoryConfirm", { name: entry.name }),
        t("peer.download"),
        {
          confirmText: t("peer.overwrite"),
          cancelText: t("confirm.cancel"),
        },
      );
      if (!overwrite) return;
      plan = await request("overwrite");
    }

    if (!plan.files.length) {
      app.setNotice("peer.downloadEmptyDirectory", {
        count: plan.directoryCount,
        path: plan.localRoot,
      });
      return;
    }

    downloadDirectoryName.value = entry.name;
    downloadRoot.value = plan.localRoot;
    downloadQueue.value = plan.files.map((file) => ({
      ...file,
      status: "pending",
      error: "",
      bytes: null,
    }));
    downloadCanceled.value = false;
    downloadRunning.value = true;
    downloadDialogOpen.value = true;
    await processDownloadQueue();
  });
}

// 批量下载同时包含文件与目录：目录用 prepare 展开成文件清单，单个文件直接
// 下载到同一目标目录。后端没有混合批量的单一命令，这里在前端编排成一个队列。
async function ensureBatchConflictPolicy(name) {
  if (batchConflictResolved) return batchConflict.value === "overwrite" ? "overwrite" : null;
  const overwrite = await confirmAction(
    t("peer.overwriteBatchConfirm", { name }),
    t("peer.overwrite"),
    { confirmText: t("peer.overwrite"), cancelText: t("confirm.cancel") },
  );
  batchConflict.value = overwrite ? "overwrite" : "fail";
  batchConflictResolved = true;
  return overwrite ? "overwrite" : null;
}

function buildPrepareRequest(entry, localParent, conflict) {
  return app.invoke("prepare_peer_directory_download", {
    peerId: props.peer.id,
    shareId: activeShareId.value,
    remotePath: entry.path,
    localParent,
    ignoreOptions: {
      respectGitignore: generalSettings.respectGitignore,
      patterns: generalSettings.ignorePatterns,
    },
    conflict,
  });
}

async function downloadBatch(entries) {
  if (!entries.length || busy.value) return;
  await run(async () => {
    const selected = await openDialog({
      title: t("peer.pickDownloadFolder"),
      multiple: false,
      directory: true,
    });
    const localParent = typeof selected === "string" ? selected : "";
    if (!localParent) return;
    // 下载已开始，选中状态不应残留在已下载的条目上。
    clearSelection();

    batchConflict.value = "fail";
    batchConflictResolved = false;
    const items = [];
    let aborted = false;
    for (const entry of entries) {
      if (entry.isDir) {
        let plan;
        try {
          plan = await buildPrepareRequest(entry, localParent, batchConflict.value);
        } catch (error) {
          if (normalizeAppError(error).code !== "TRANSFER_CONFLICT") throw error;
          const policy = await ensureBatchConflictPolicy(entry.name);
          if (!policy) {
            aborted = true;
            break;
          }
          plan = await buildPrepareRequest(entry, localParent, policy);
        }
        items.push(
          ...plan.files.map((file) => ({ ...file, status: "pending", error: "", bytes: null })),
        );
      } else {
        items.push({
          remotePath: entry.path,
          localDir: localParent,
          relativePath: entry.name,
          size: entry.size,
          status: "pending",
          error: "",
          bytes: null,
        });
      }
    }

    if (aborted) return;
    if (!items.length) {
      app.setNotice("peer.downloadEmptySelection", {});
      return;
    }
    downloadDirectoryName.value = t("peer.selectedCount", { count: entries.length });
    downloadRoot.value = localParent;
    downloadQueue.value = items;
    downloadCanceled.value = false;
    downloadRunning.value = true;
    downloadDialogOpen.value = true;
    await processDownloadQueue();
  });
}

function downloadSelected() {
  const entries = selectedEntries.value;
  if (!entries.length || busy.value) return;
  if (entries.length === 1) {
    const entry = entries[0];
    if (entry.isDir) void downloadDirectory(entry);
    else void downloadEntry(entry);
    return;
  }
  void downloadBatch(entries);
}

async function revealSavedFile() {
  if (!savedPath.value) return;
  try {
    await app.invoke("reveal_local_file", { path: savedPath.value });
    savedOpen.value = false;
  } catch (error) {
    app.handleFailure(error);
  }
}

// 批量/目录下载完成后打开本地根目录，与单文件下载的入口保持一致。
// 根目录是文件夹，reveal_local_file 只接受普通文件，因此用目录版命令。
async function revealDownloadRoot() {
  if (!downloadRoot.value) return;
  try {
    await app.invoke("reveal_local_directory", { path: downloadRoot.value });
    downloadDialogOpen.value = false;
  } catch (error) {
    app.handleFailure(error);
  }
}

async function openSavedFile() {
  if (!savedPath.value) return;
  try {
    await app.invoke("open_local_file", { path: savedPath.value });
    savedOpen.value = false;
  } catch (error) {
    app.handleFailure(error);
  }
}

async function chooseUploadFiles() {
  if (!activeShareId.value || busy.value) return;
  await run(async () => {
    const selected = await openDialog({
      title: t("peer.pickUploadFiles"),
      multiple: true,
      directory: false,
    });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    if (paths.length) await prepareUploadQueue(paths);
  });
}

async function chooseUploadFolder() {
  if (!activeShareId.value || busy.value) return;
  await run(async () => {
    const selected = await openDialog({
      title: t("peer.pickUploadFolder"),
      multiple: false,
      directory: true,
    });
    const paths = typeof selected === "string" ? [selected] : [];
    if (paths.length) await prepareUploadQueue(paths);
  });
}

async function prepareUploadQueue(paths) {
  const candidates = await app.invoke("collect_upload_paths", {
    paths,
    ignoreOptions: { respectGitignore: generalSettings.respectGitignore, patterns: generalSettings.ignorePatterns },
  });
  if (!candidates.length) {
    app.setNotice("peer.noUploadFiles");
    return;
  }
  uploadQueue.value = candidates.map((candidate) => ({
    ...candidate,
    status: "pending",
    error: "",
  }));
  uploadCanceled.value = false;
  uploadRunning.value = true;
  uploadDialogOpen.value = true;
  await processUploadQueue();
}

function markRemainingCanceled() {
  for (const item of uploadQueue.value) {
    if (item.status === "pending") {
      item.status = "canceled";
    }
  }
}

async function uploadQueueItem(item, remoteDir, conflict) {
  const transferId = await app.invoke("begin_peer_upload");
  uploadTransferId.value = transferId;
  app.setTransferProgress({
    transferId,
    direction: "upload",
    phase: "hashing",
    transferred: 0,
    total: item.size,
    bytesPerSecond: 0,
  }, true);
  try {
    if (uploadCanceled.value) await app.invoke("cancel_peer_upload", { transferId });
    return await app.invoke("upload_peer_file", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      localPath: item.localPath,
      remoteDir,
      conflict,
      transferId,
    });
  } finally {
    app.clearTransfer?.(transferId);
    if (uploadTransferId.value === transferId) uploadTransferId.value = "";
  }
}

async function processUploadQueue() {
  let completed = 0;
  for (const item of uploadQueue.value) {
    if (uploadCanceled.value) {
      if (item.status === "pending") item.status = "canceled";
      continue;
    }
    if (item.status !== "pending") continue;

    item.status = "uploading";
    const remoteDir = remoteDirFor(item.relativePath);
    try {
      await uploadQueueItem(item, remoteDir, "fail");
      item.status = "done";
      completed += 1;
    } catch (error) {
      const normalized = normalizeAppError(error);
      if (normalized.code === "TRANSFER_CANCELED") {
        item.status = "canceled";
        markRemainingCanceled();
        break;
      }
      if (normalized.code !== "TRANSFER_CONFLICT") {
        item.status = "error";
        item.error = toUserMessage(normalized);
        markRemainingCanceled();
        app.handleFailure(error);
        break;
      }

      const overwrite = await confirmAction(
        t("peer.overwriteConfirm", { name: baseName(item.relativePath) }),
        t("peer.upload"),
        {
          confirmText: t("peer.overwrite"),
          cancelText: t("peer.skip"),
        },
      );
      if (uploadCanceled.value) {
        item.status = "canceled";
        continue;
      }
      if (!overwrite) {
        item.status = "skipped";
        continue;
      }
      try {
        await uploadQueueItem(item, remoteDir, "overwrite");
        item.status = "done";
        completed += 1;
      } catch (retryError) {
        if (normalizeAppError(retryError).code === "TRANSFER_CANCELED") {
          item.status = "canceled";
          markRemainingCanceled();
          break;
        }
        item.status = "error";
        item.error = toUserMessage(retryError);
        markRemainingCanceled();
        app.handleFailure(retryError);
        break;
      }
    }
  }
  uploadRunning.value = false;
  if (browserDisposed) return;
  await loadPath(peerPath.value);
  if (!uploadCanceled.value && completed > 0) {
    app.setNotice("peer.uploadFinished", { count: completed });
  }
}

async function cancelUploadQueue() {
  if (!uploadRunning.value || uploadCanceled.value) return;
  uploadCanceled.value = true;
  markRemainingCanceled();
  const transferId = uploadTransferId.value;
  if (!transferId) return;
  try {
    await app.invoke("cancel_peer_upload", { transferId });
  } catch (error) {
    uploadCanceled.value = false;
    app.handleFailure(error);
  }
}

function uploadStatusText(item) {
  if (item.status === "uploading") {
    return t({ hashing: "peer.uploadHashing", committing: "peer.uploadCommitting",
      completed: "peer.uploadCommitting", uploading: "peer.uploadStatusUploading" }[uploadPhase.value]);
  }
  const key = {
    pending: "peer.uploadStatusPending",
    uploading: "peer.uploadStatusUploading",
    done: "peer.uploadStatusDone",
    skipped: "peer.uploadStatusSkipped",
    canceled: "peer.uploadStatusCanceled",
    error: "peer.uploadStatusError",
  }[item.status];
  return key ? t(key) : "";
}

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

async function loadPreviewRenderTools() {
  if (previewRenderTools.value) return previewRenderTools.value;
  previewRenderToolsPromise ??= import("../lib/textPreviewRender");
  try {
    previewRenderTools.value = await previewRenderToolsPromise;
  } catch (error) {
    previewRenderToolsPromise = null;
    throw error;
  }
  return previewRenderTools.value;
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
      if (
        preview.encoding === "utf8" &&
        ["code", "markdown"].includes(detectPreviewFormat(entry))
      ) {
        await loadPreviewRenderTools();
      }
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

function formatBytes(size) {
  const bytes = Number(size) || 0;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
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
                  :disabled="busy || !activeShareId"
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
                  :disabled="busy || !activeShareId"
                  @click="chooseUploadFolder"
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="M3.5 7.5c0-.8.7-1.5 1.5-1.5h4l1.8 2H19c.8 0 1.5.7 1.5 1.5v8c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z" />
                    <path d="M12 17v-5M9.5 14.5 12 12l2.5 2.5" />
                  </svg>
                </button>
              </el-tooltip>
            </div>
          </div>
        </Teleport>

        <p v-if="peerNotice" class="browser-notice">{{ peerNotice }}</p>

        <div
          v-if="peerListing"
          ref="fileViewEl"
          class="file-view"
          tabindex="0"
          @keydown="onFileViewKeydown"
          @mousedown="onFileViewMousedown"
        >
          <div v-if="marquee" class="marquee" :style="marqueeStyle"></div>
          <div v-if="viewMode === 'icons'" class="icon-grid">
            <button
              v-for="entry in peerListing.entries"
              :key="entry.path"
              type="button"
              class="file-tile"
              :data-path="entry.path"
              :class="{ 'is-selected': isSelected(entry) }"
              @click="selectEntry(entry, $event)"
              @dblclick="openEntry(entry)"
            >
              <FileGlyph :entry="entry" size="lg" />
              <span class="file-tile__name">{{ entry.name }}</span>
              <span class="file-tile__meta">{{ entryKind(entry) }}</span>
            </button>
          </div>

          <div v-else-if="viewMode === 'list'" class="file-table-wrap">
            <table class="file-table">
            <thead>
              <tr>
                <th scope="col">{{ t("peer.name") }}</th>
                <th scope="col">{{ t("peer.kind") }}</th>
                <th scope="col">{{ t("peer.size") }}</th>
                <th scope="col">{{ t("peer.modified") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="entry in peerListing.entries"
                :key="entry.path"
                tabindex="0"
                :data-path="entry.path"
                :class="{ 'is-selected': isSelected(entry) }"
                @click="selectEntry(entry, $event)"
                @dblclick="openEntry(entry)"
                @keydown.enter="openEntry(entry)"
              >
                <td>
                  <span class="table-name">
                    <FileGlyph :entry="entry" />
                    <span>{{ entry.name }}</span>
                  </span>
                </td>
                <td>{{ entryKind(entry) }}</td>
                <td>{{ formatSize(entry) }}</td>
                <td>{{ formatModified(entry.modified) }}</td>
              </tr>
            </tbody>
            </table>
          </div>

          <div v-else-if="viewMode === 'columns'" class="column-view" ref="columnViewEl">
            <div
              v-for="(column, index) in columnStack"
              :key="column.path"
              class="column"
            >
              <button
                v-for="entry in column.entries"
                :key="entry.path"
                type="button"
                class="column-row"
                :data-path="entry.path"
                :class="{ 'is-selected': isSelected(entry) }"
                @click="loadColumn(entry, index)"
                @dblclick="previewEntry(entry)"
              >
                <FileGlyph :entry="entry" />
                <span class="column-name">{{ entry.name }}</span>
                <span v-if="entry.isDir" class="column-chevron">›</span>
              </button>
              <p v-if="!column.entries.length" class="column-empty">
                {{ t("peer.emptyDir") }}
              </p>
            </div>
          </div>

          <div v-else class="gallery-view" data-testid="gallery-view">
            <div class="gallery-stage">
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
            </div>
            <div class="gallery-strip">
              <button
                v-for="entry in peerListing.entries"
                :key="entry.path"
                type="button"
                class="gallery-thumb"
                :data-path="entry.path"
                :class="{ 'is-selected': isSelected(entry) }"
                @click="selectEntry(entry, $event)"
                @dblclick="openEntry(entry)"
              >
                <FileGlyph :entry="entry" />
                <span>{{ entry.name }}</span>
              </button>
            </div>
          </div>

          <p v-if="!peerListing.entries.length" class="file-empty">
            {{ t("peer.emptyDir") }}
          </p>
        </div>

        <!-- 加载中先清空旧内容并给出即时反馈；失败时提示可点击地址栏重试。 -->
        <div v-else class="file-view file-view--placeholder" :aria-busy="navigating">
          <div v-if="navigating" class="file-loading">
            <span class="file-loading__spinner" aria-hidden="true"></span>
            <span>{{ t("peer.loading") }}</span>
          </div>
          <p v-else class="file-empty">{{ t("peer.loadFailed") }}</p>
        </div>

        <div
          v-if="selectedEntries.length || viewMode === 'gallery'"
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
                type="primary"
                :disabled="busy"
                @click="browse(selectedEntry.path)"
              >
                {{ t("peer.open") }}
              </el-button>
              <el-button plain :disabled="busy" @click="downloadDirectory(selectedEntry)">
                {{ t("peer.download") }}
              </el-button>
            </template>
            <template v-else-if="selectedEntries.length === 1">
              <el-button plain :disabled="busy" @click="downloadEntry(selectedEntry)">
                {{ t("peer.download") }}
              </el-button>
              <el-button
                v-if="isReadableText(selectedEntry)"
                plain
                :disabled="busy"
                @click="editEntry(selectedEntry)"
              >
                {{ t("peer.edit") }}
              </el-button>
            </template>
            <el-button v-else type="primary" plain :disabled="busy" @click="downloadSelected">
              {{ t("peer.download") }}
            </el-button>
            <el-button
              v-if="canDeleteEntries && selectedEntries.length === 1"
              type="danger"
              plain
              :disabled="busy"
              @click="deleteEntry(selectedEntry)"
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

    <el-dialog
      v-model="downloadOpen"
      :title="t('peer.downloading')"
      width="420"
      :show-close="false"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <div class="transfer-dialog">
        <p class="transfer-dialog__name">{{ downloadName }}</p>
        <el-progress
          :percentage="downloadPercent"
          :indeterminate="!activeTransfer?.total"
          :stroke-width="8"
        />
        <p class="hint">
          <template v-if="activeTransfer">
            {{ formatBytes(activeTransfer.transferred) }}
            <template v-if="activeTransfer.total">
              / {{ formatBytes(activeTransfer.total) }}
            </template>
          </template>
          <template v-else>{{ t("peer.preparingDownload") }}</template>
        </p>
      </div>
    </el-dialog>

    <el-dialog
      v-model="downloadDialogOpen"
      :title="t('peer.downloadQueue')"
      width="560"
      :show-close="!downloadRunning"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <div class="transfer-dialog">
        <p class="transfer-dialog__name">{{ downloadDirectoryName }}</p>
        <p class="saved-path">{{ downloadRoot }}</p>
        <p class="hint">{{ downloadSummary }}</p>
        <el-progress :percentage="downloadQueuePercent" :stroke-width="8" />
        <p v-if="downloadTotalBytes" class="hint">
          {{ formatBytes(downloadTransferredBytes) }} /
          {{ formatBytes(downloadTotalBytes) }}
        </p>
        <ul class="transfer-queue">
          <li
            v-for="item in downloadQueue"
            :key="item.remotePath"
          >
            <div class="transfer-queue__name">
              <FileGlyph
                :entry="{
                  isDir: false,
                  name: item.relativePath,
                  contentType: null,
                }"
              />
              <div>
                <strong>{{ baseName(item.relativePath) }}</strong>
                <span>{{ item.relativePath }} · {{ formatBytes(item.size) }}</span>
              </div>
            </div>
            <el-tag
              class="row-tag"
              :type="statusTagTone(item.status)"
              effect="plain"
              round
              size="small"
              :title="item.error || undefined"
            >
              {{ downloadStatusText(item) }}
            </el-tag>
          </li>
        </ul>
      </div>
      <template #footer>
        <el-button
          v-if="downloadRunning"
          plain
          :disabled="downloadCanceled"
          @click="cancelDownloadQueue"
        >
          {{ downloadCanceled ? t("peer.cancelingDownload") : t("peer.cancelDownload") }}
        </el-button>
        <template v-else>
          <el-button v-if="downloadRoot" plain @click="revealDownloadRoot">
            {{ t("peer.revealInFolder") }}
          </el-button>
          <el-button type="primary" @click="downloadDialogOpen = false">
            {{ t("common.close") }}
          </el-button>
        </template>
      </template>
    </el-dialog>

    <el-dialog
      v-model="savedOpen"
      :title="t('peer.downloadComplete')"
      width="480"
      :close-on-click-modal="false"
    >
      <p class="saved-path">{{ t("peer.savedTo", { path: savedPath }) }}</p>
      <template #footer>
        <el-button plain @click="revealSavedFile">
          {{ t("peer.revealInFolder") }}
        </el-button>
        <el-button type="primary" @click="openSavedFile">
          {{ t("peer.openFile") }}
        </el-button>
      </template>
    </el-dialog>

    <el-dialog
      v-model="uploadDialogOpen"
      :title="t('peer.uploadQueue')"
      width="560"
      :show-close="!uploadRunning"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <div class="transfer-dialog">
        <p class="hint">{{ uploadSummary }}</p>
        <el-progress :percentage="uploadPercent" :stroke-width="8" />
        <div v-if="currentUpload" class="upload-current">
          <strong>{{ baseName(currentUpload.relativePath) }}</strong>
          <p class="hint" role="status">
            {{ uploadStatusText(currentUpload) }}
            <template v-if="uploadTransfer && uploadPhase !== 'committing' && uploadPhase !== 'completed'">
              · {{ uploadFilePercent }}%
            </template>
          </p>
          <el-progress
            :percentage="uploadFilePercent"
            :indeterminate="!uploadTransfer || uploadPhase === 'committing' || uploadPhase === 'completed'"
            :show-text="false"
            :aria-label="baseName(currentUpload.relativePath)"
            :stroke-width="6"
          />
          <p v-if="uploadPhase === 'committing' || uploadPhase === 'completed'" class="hint">{{ t("peer.uploadCommitHint") }}</p>
          <p v-if="uploadTransfer" class="hint upload-current__metrics">
            {{ formatBytes(uploadTransfer.transferred) }} / {{ formatBytes(uploadTransfer.total) }}
            <span v-if="uploadPhase === 'uploading'"> · {{ formatBytes(uploadSpeed) }}/s</span>
          </p>
        </div>
        <ul class="transfer-queue">
          <li
            v-for="(item, index) in uploadQueue"
            :key="`${item.localPath}-${index}`"
          >
            <div class="transfer-queue__name">
              <FileGlyph
                :entry="{
                  isDir: false,
                  name: item.relativePath,
                  contentType: null,
                }"
              />
              <div>
                <strong>{{ baseName(item.relativePath) }}</strong>
                <span>{{ item.relativePath }} · {{ formatBytes(item.size) }}</span>
              </div>
            </div>
            <el-tag
              class="row-tag"
              :type="statusTagTone(item.status)"
              effect="plain"
              round
              size="small"
              :title="item.error || undefined"
            >
              {{ uploadStatusText(item) }}
            </el-tag>
          </li>
        </ul>
      </div>
      <template #footer>
        <el-button
          v-if="uploadRunning"
          plain
          :disabled="uploadCanceled"
          @click="cancelUploadQueue"
        >
          {{ uploadCanceled ? t("peer.cancelingUpload") : t("peer.cancelUpload") }}
        </el-button>
        <el-button v-else type="primary" @click="uploadDialogOpen = false">
          {{ t("common.close") }}
        </el-button>
      </template>
    </el-dialog>

    <section
      v-if="previewOpen"
      ref="previewDialogEl"
      class="preview-dialog"
      role="dialog"
      aria-modal="true"
      :aria-label="previewTarget?.name ?? t('peer.preview')"
      tabindex="-1"
      @keydown.esc="closePreview"
    >
      <header class="preview-dialog__head">
        <div class="preview-dialog__identity">
          <FileGlyph
            v-if="previewTarget"
            :entry="previewTarget"
          />
          <div>
            <strong>{{ previewTarget?.name ?? activePreview?.fileName }}</strong>
            <span>{{ previewMeta }}</span>
          </div>
        </div>
        <div class="preview-dialog__actions">
          <span v-if="codePreview" class="preview-language">
            {{ codePreview.language }}
          </span>
          <el-button text @click="closePreview">
            {{ t("common.close") }}
          </el-button>
        </div>
      </header>

      <div class="preview-dialog__body">
        <div
          v-if="previewKind === 'unavailable'"
          class="preview-unavailable"
        >
          <FileGlyph v-if="previewTarget" :entry="previewTarget" size="xl" />
          <strong>{{ t("peer.previewUnavailable") }}</strong>
          <p>{{ previewLimitText }}</p>
          <el-button
            type="primary"
            :disabled="busy"
            @click="downloadEntry(previewTarget)"
          >
            {{ t("peer.download") }}
          </el-button>
        </div>

        <div v-else-if="previewKind === 'image'" class="preview-image-wrap">
          <img
            :src="peerPreviewSource"
            :alt="t('peer.previewAlt')"
            class="preview-image-full"
          />
        </div>

        <div v-else-if="previewKind === 'audio'" class="preview-media-wrap">
          <audio
            :src="peerPreviewSource"
            class="preview-audio"
            controls
            preload="metadata"
          ></audio>
        </div>

        <div v-else-if="previewKind === 'video'" class="preview-media-wrap">
          <video
            :src="peerPreviewSource"
            class="preview-video"
            controls
            playsinline
            preload="metadata"
          ></video>
        </div>

        <div
          v-else-if="previewKind === 'markdown'"
          class="markdown-body"
          v-html="markdownPreviewHtml"
        ></div>

        <div v-else-if="previewKind === 'table'" class="preview-table-wrap">
          <p
            v-if="!tablePreview?.header.length && !tablePreview?.rows.length"
            class="preview-empty"
          >
            {{ t("peer.previewEmpty") }}
          </p>
          <table v-else class="preview-table">
            <thead>
              <tr>
                <th class="preview-row-number" scope="col">#</th>
                <th
                  v-for="column in tablePreview.columnCount"
                  :key="column"
                  scope="col"
                >
                  {{
                    tablePreview.header[column - 1] ||
                    t("peer.columnName", { number: column })
                  }}
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="(row, rowIndex) in tablePreview.rows"
                :key="rowIndex"
              >
                <th class="preview-row-number" scope="row">
                  {{ rowIndex + 1 }}
                </th>
                <td
                  v-for="column in tablePreview.columnCount"
                  :key="column"
                >
                  {{ row[column - 1] ?? "" }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <div v-else-if="previewKind === 'code'" class="preview-code-wrap">
          <pre class="preview-code-gutter" aria-hidden="true"><span
            v-for="line in codeLineCount"
            :key="line"
          >{{ line }}</span></pre>
          <pre class="preview-code"><code class="hljs" v-html="codePreview?.html"></code></pre>
        </div>

        <pre
          v-else-if="previewKind === 'text'"
          class="preview-text-full"
        >{{ activePreview?.data }}</pre>

        <div
          v-else-if="previewKind === 'binary'"
          class="preview-empty-state"
        >
          <p>{{ t("peer.binaryPreview") }}</p>
        </div>
      </div>
    </section>
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

.browser-notice {
  margin: 0;
  padding: 8px 10px;
  border-radius: 8px;
  background: var(--app-surface-2);
  color: var(--app-text-secondary);
  font-size: 12.5px;
}

/* Keep the pathbar and selection bar fixed; each view owns its scrolling region. */
.browser-main > .file-view {
  flex: 1;
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

.browser-main > .file-view > .icon-grid,
.browser-main > .file-view > .file-table-wrap,
.browser-main > .file-view > .column-view,
.browser-main > .file-view > .gallery-view {
  height: 100%;
  min-height: 0;
}

.icon-grid {
  overflow: auto;
  align-content: start;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(112px, 1fr));
  gap: 10px;
}

.file-tile {
  min-height: 106px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 5px;
  padding: 12px 8px;
  border: 1px solid transparent;
  border-radius: 10px;
  background: transparent;
  color: var(--app-text);
  font: inherit;
  cursor: pointer;
}

.file-tile:hover {
  background: var(--app-surface-2);
}

.file-tile.is-selected {
  border-color: var(--selection-border);
  background: var(--selection-bg);
}

.file-tile__name {
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12.5px;
  font-weight: 500;
}

.file-tile__meta {
  font-size: 11px;
  color: var(--app-text-muted);
}

.file-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}

.file-table th {
  position: sticky;
  top: 0;
  z-index: 2;
  padding: 7px 9px;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-surface);
  color: var(--app-text-muted);
  font-size: 11.5px;
  font-weight: 600;
  text-align: left;
}

.file-table td {
  padding: 7px 9px;
  /* 行分隔线用文字色按比例调出，亮色和暗色下都比 --app-border 更清晰，
     多选行的背景之上也能保持可辨。 */
  border-bottom: 1px solid color-mix(in oklab, var(--app-text) 20%, var(--app-surface));
  color: var(--app-text-secondary);
  white-space: nowrap;
}

.file-table tbody tr {
  cursor: default;
}

.file-table tbody tr:hover {
  background: var(--app-surface-2);
}

.file-table tbody tr.is-selected {
  background: var(--selection-bg);
}

.table-name {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--app-text);
}

.file-table-wrap {
  overflow: auto;
}

.column-view {
  display: flex;
  align-items: stretch;
  overflow: hidden;
}

.column {
  width: 240px;
  flex: none;
  overflow-y: auto;
  overflow-y: auto;
  padding: 5px;
  border-right: 1px solid var(--app-border);
}

.column:last-child {
  border-right: 0;
}

.column-row {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 6px 8px;
  border: 0;
  border-radius: 7px;
  background: transparent;
  color: var(--app-text-secondary);
  font: inherit;
  font-size: 12.5px;
  text-align: left;
  cursor: pointer;
}

.column-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.column-row:hover {
  background: var(--app-surface-2);
}

.column-row.is-selected {
  color: var(--app-text);
  background: var(--selection-bg);
}

.column-chevron {
  flex: none;
  color: var(--app-text-muted);
  font-size: 15px;
}

.column-empty {
  margin: 8px;
  font-size: 12px;
  color: var(--app-text-muted);
}

/* Keep the file list visible above the selection bar. */
.gallery-view {
  box-sizing: border-box;
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.gallery-stage {
  flex: 1;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 16px;
  border: 1px solid var(--app-border);
  border-radius: 12px;
  background:
    linear-gradient(
      45deg,
      color-mix(in oklab, var(--app-surface-2) 55%, transparent) 25%,
      transparent 25%,
      50%,
      color-mix(in oklab, var(--app-surface-2) 55%, transparent) 50%,
      75%,
      transparent 75%
    ),
    var(--app-surface-3);
  background-size: 16px 16px;
}

.gallery-stage > * {
  max-width: 100%;
  max-height: 100%;
  min-width: 0;
  min-height: 0;
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

.gallery-strip {
  flex: none;
  display: flex;
  gap: 8px;
  overflow-x: auto;
  padding: 2px;
}

.gallery-thumb {
  width: 96px;
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 7px 5px;
  border: 1px solid transparent;
  border-radius: 8px;
  background: transparent;
  color: var(--app-text-secondary);
  font: inherit;
  cursor: pointer;
}

.gallery-thumb span {
  width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 11px;
  text-align: center;
}

.gallery-thumb:hover {
  background: var(--app-surface-2);
}

.gallery-thumb.is-selected {
  border-color: var(--selection-border);
  background: var(--selection-bg);
}

.file-empty {
  margin: 22px 0;
  text-align: center;
  font-size: 13px;
  color: var(--app-text-muted);
}

.file-view--placeholder {
  display: flex;
  align-items: center;
  justify-content: center;
}

.file-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--app-text-muted);
  font-size: 13px;
}

.file-loading__spinner {
  width: 16px;
  height: 16px;
  flex: none;
  border: 2px solid var(--app-border);
  border-top-color: var(--el-color-primary);
  border-radius: 50%;
  animation: file-loading-spin 0.8s linear infinite;
}

@keyframes file-loading-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .file-loading__spinner {
    animation: none;
  }
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

.preview-dialog {
  position: absolute;
  inset: 0;
  z-index: 40;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background: var(--app-surface);
  color: var(--app-text);
  outline: none;
}

.preview-dialog__head {
  flex: none;
  min-height: 54px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 8px 14px;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-surface-2);
}

.preview-dialog__identity {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 10px;
}

.preview-dialog__identity > div {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.preview-dialog__identity strong,
.preview-dialog__identity span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.preview-dialog__identity strong {
  font-size: 13px;
  color: var(--app-text);
}

.preview-dialog__identity span {
  font-size: 11.5px;
  color: var(--app-text-muted);
}

.preview-dialog__actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 10px;
}

.preview-language {
  padding: 3px 7px;
  border-radius: 6px;
  background: var(--pill-bg);
  color: var(--pill-fg);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 10.5px;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.preview-dialog__body {
  flex: 1;
  min-height: 0;
  overflow: auto;
  background: var(--app-surface);
}

.preview-image-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.preview-image-full {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  border-radius: 8px;
  border: 1px solid var(--app-border);
  background: var(--app-surface-2);
}

.preview-media-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.preview-audio {
  width: min(560px, 100%);
}

.preview-video {
  max-width: 100%;
  max-height: 100%;
  border-radius: 10px;
  background: #000;
  box-shadow: 0 12px 36px rgba(22, 32, 46, 0.2);
}

.markdown-body {
  max-width: 900px;
  margin: 0 auto;
  padding: 32px 40px 64px;
  color: var(--app-text-secondary);
  font-size: 14px;
  line-height: 1.75;
  overflow-wrap: anywhere;
}

.markdown-body :deep(h1),
.markdown-body :deep(h2),
.markdown-body :deep(h3),
.markdown-body :deep(h4) {
  margin: 1.5em 0 0.6em;
  color: var(--app-text);
  line-height: 1.3;
}

.markdown-body :deep(h1) {
  font-size: 1.8em;
  padding-bottom: 0.35em;
  border-bottom: 1px solid var(--app-border);
}

.markdown-body :deep(h2) {
  font-size: 1.4em;
  padding-bottom: 0.3em;
  border-bottom: 1px solid color-mix(in oklab, var(--app-border) 65%, transparent);
}

.markdown-body :deep(h3) {
  font-size: 1.18em;
}

.markdown-body :deep(p),
.markdown-body :deep(ul),
.markdown-body :deep(ol),
.markdown-body :deep(blockquote),
.markdown-body :deep(pre),
.markdown-body :deep(table) {
  margin: 0 0 1em;
}

.markdown-body :deep(ul),
.markdown-body :deep(ol) {
  padding-left: 1.6em;
}

.markdown-body :deep(li + li) {
  margin-top: 0.25em;
}

.markdown-body :deep(a) {
  color: var(--accent);
  text-decoration: none;
}

.markdown-body :deep(a:hover) {
  text-decoration: underline;
}

.markdown-body :deep(code) {
  padding: 0.18em 0.36em;
  border-radius: 5px;
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 0.88em;
  color: var(--app-text);
}

.markdown-body :deep(pre) {
  padding: 14px 16px;
  overflow: auto;
  border-radius: 8px;
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
}

.markdown-body :deep(pre code) {
  padding: 0;
  border: 0;
  background: transparent;
  font-size: 0.86em;
}

.markdown-body :deep(blockquote) {
  padding: 0.2em 0 0.2em 1em;
  border-left: 3px solid var(--app-border);
  color: var(--app-text-muted);
}

.markdown-body :deep(hr) {
  height: 1px;
  margin: 1.5em 0;
  border: 0;
  background: var(--app-border);
}

.markdown-body :deep(img) {
  max-width: 100%;
  border-radius: 8px;
}

.markdown-body :deep(table) {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.92em;
}

.markdown-body :deep(th),
.markdown-body :deep(td) {
  padding: 7px 10px;
  border: 1px solid var(--app-border);
  text-align: left;
  vertical-align: top;
}

.markdown-body :deep(th) {
  background: var(--app-surface-2);
  color: var(--app-text);
}

.preview-table-wrap {
  min-width: 100%;
  padding-bottom: 24px;
}

.preview-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}

.preview-table th,
.preview-table td {
  max-width: 420px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--app-border);
  text-align: left;
  vertical-align: top;
  white-space: pre-wrap;
  word-break: break-word;
}

.preview-table thead th {
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--app-surface-2);
  color: var(--app-text);
  font-size: 11.5px;
  font-weight: 600;
  border-bottom-color: var(--app-border);
}

.preview-table tbody tr:nth-child(even) {
  background: color-mix(in oklab, var(--app-surface-2) 55%, transparent);
}

.preview-table .preview-row-number {
  width: 1%;
  padding-right: 10px;
  color: var(--app-text-muted);
  font-weight: 500;
  text-align: right;
  white-space: nowrap;
  user-select: none;
}

.preview-code-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: stretch;
  overflow: auto;
}

.preview-code-gutter {
  position: sticky;
  left: 0;
  z-index: 1;
  flex: none;
  box-sizing: border-box;
  margin: 0;
  padding: 20px 12px 48px 16px;
  border-right: 1px solid var(--app-border);
  background: var(--app-surface);
  color: var(--app-text-muted);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.65;
  text-align: right;
  user-select: none;
}

.preview-code-gutter span {
  display: block;
}

.preview-code {
  flex: 1 0 auto;
  min-width: max-content;
  min-height: 100%;
  box-sizing: border-box;
  margin: 0;
  padding: 20px 22px 48px;
  color: var(--app-text);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.65;
  tab-size: 2;
  white-space: pre;
}

.preview-code code {
  font-family: inherit;
}

.preview-text-full {
  min-height: 100%;
  box-sizing: border-box;
  margin: 0;
  padding: 24px 28px 48px;
  color: var(--app-text-secondary);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.preview-unavailable,
.preview-empty-state {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 32px;
  text-align: center;
}

.preview-unavailable strong {
  font-size: 16px;
  color: var(--app-text);
}

.preview-unavailable p,
.preview-empty-state p,
.preview-empty {
  max-width: 480px;
  margin: 0;
  color: var(--app-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}

.preview-empty {
  padding: 32px;
  text-align: center;
}

.preview-code :deep(.hljs-comment),
.preview-code :deep(.hljs-quote),
.preview-code :deep(.hljs-meta) {
  color: var(--syntax-comment);
}

.preview-code :deep(.hljs-keyword),
.preview-code :deep(.hljs-literal),
.preview-code :deep(.hljs-selector-tag),
.preview-code :deep(.hljs-doctag) {
  color: var(--syntax-keyword);
}

.preview-code :deep(.hljs-string),
.preview-code :deep(.hljs-attr),
.preview-code :deep(.hljs-attribute),
.preview-code :deep(.hljs-addition),
.preview-code :deep(.hljs-template-tag) {
  color: var(--syntax-string);
}

.preview-code :deep(.hljs-number),
.preview-code :deep(.hljs-symbol),
.preview-code :deep(.hljs-regexp) {
  color: var(--syntax-number);
}

.preview-code :deep(.hljs-title),
.preview-code :deep(.hljs-section),
.preview-code :deep(.hljs-name),
.preview-code :deep(.hljs-selector-id),
.preview-code :deep(.hljs-selector-class) {
  color: var(--syntax-title);
}

.preview-code :deep(.hljs-built_in),
.preview-code :deep(.hljs-type) {
  color: var(--syntax-built-in);
}

.preview-code :deep(.hljs-variable),
.preview-code :deep(.hljs-template-variable),
.preview-code :deep(.hljs-params) {
  color: var(--syntax-variable);
}

.preview-code :deep(.hljs-deletion) {
  color: var(--syntax-deletion);
}

.preview-code :deep(.hljs-emphasis) {
  font-style: italic;
}

.preview-code :deep(.hljs-strong) {
  font-weight: 700;
}

.edit-panel p {
  margin: 4px 0 0;
}

.transfer-dialog {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.upload-current {
  display: grid;
  gap: 8px;
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  overflow-wrap: anywhere;
}

.upload-current p { margin: 0; }
.upload-current__metrics { font-variant-numeric: tabular-nums; }

.transfer-dialog__name {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  color: var(--app-text);
  overflow-wrap: anywhere;
}

.saved-path {
  margin: 0;
  font-size: 13px;
  line-height: 1.65;
  color: var(--app-text-secondary);
  overflow-wrap: anywhere;
}

.transfer-queue {
  list-style: none;
  max-height: 320px;
  overflow-y: auto;
  margin: 0;
  padding: 0;
  border: 1px solid var(--app-border);
  border-radius: 10px;
}

.transfer-queue li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 9px 11px;
}

.transfer-queue li + li {
  border-top: 1px solid var(--app-border);
}

.transfer-queue__name {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 9px;
}

.transfer-queue__name > div {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.transfer-queue__name strong,
.transfer-queue__name span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.transfer-queue__name strong {
  font-size: 12.5px;
  color: var(--app-text);
}

.transfer-queue__name span {
  font-size: 11px;
  color: var(--app-text-muted);
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
