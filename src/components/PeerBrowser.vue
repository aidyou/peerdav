<script setup>
import { computed, ref, watch } from "vue";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

import FileGlyph from "./FileGlyph.vue";
import { useAppContext } from "../lib/appContext";
import { confirmAction } from "../lib/confirm";
import { fileKind, fileKindKey } from "../lib/fileKinds";
import { t } from "../lib/i18n";
import { normalizeAppError } from "../lib/invoke";
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
const peerEdit = ref(null);
const peerNoticeState = ref(null);
const viewMode = ref(readViewMode());
const columnStack = ref([]);
const selectedEntry = ref(null);
const downloadOpen = ref(false);
const downloadName = ref("");
const downloadTransferId = ref("");
const savedOpen = ref(false);
const savedPath = ref("");
const uploadDialogOpen = ref(false);
const uploadQueue = ref([]);
const uploadRunning = ref(false);
const uploadCanceled = ref(false);

const activeShare = computed(
  () => peerShares.value.find((share) => share.id === activeShareId.value) ?? null,
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

const uploadFinished = computed(
  () =>
    uploadQueue.value.filter(
      (item) => !["pending", "uploading"].includes(item.status),
    ).length,
);

const uploadPercent = computed(() => {
  if (!uploadQueue.value.length) return 0;
  return Math.round((uploadFinished.value / uploadQueue.value.length) * 100);
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
  }
  if (mode === "gallery" && galleryFile.value && !activePreview.value) {
    void previewEntry(galleryFile.value);
  }
}

function setPeerNotice(key, params) {
  peerNoticeState.value = { key, params };
}

function reset() {
  peerShares.value = [];
  activeShareId.value = "";
  peerPath.value = "/";
  peerListing.value = null;
  peerPreview.value = null;
  peerPreviewPath.value = "";
  peerEdit.value = null;
  peerNoticeState.value = null;
  columnStack.value = [];
  selectedEntry.value = null;
  downloadOpen.value = false;
  downloadTransferId.value = "";
  savedOpen.value = false;
  savedPath.value = "";
  uploadDialogOpen.value = false;
  uploadQueue.value = [];
  uploadRunning.value = false;
  uploadCanceled.value = false;
}

function applyListing(listing) {
  peerListing.value = listing;
  peerPath.value = listing.path;
  selectedEntry.value = null;
  peerPreview.value = null;
  peerPreviewPath.value = "";
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

async function loadPath(path) {
  const listing = await fetchListing(path);
  applyListing(listing);
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
  if (busy.value) return;
  activeShareId.value = shareId;
  selectedEntry.value = null;
  await run(() => loadPath("/"));
}

async function browse(path) {
  if (!props.peer.id || !activeShareId.value || busy.value) return;
  await run(() => loadPath(path));
}

async function refreshListing() {
  if (!activeShareId.value || busy.value) return;
  await run(() => loadPath(peerPath.value));
}

function selectEntry(entry) {
  selectedEntry.value = entry;
  if (viewMode.value === "gallery" && !entry.isDir) {
    void previewEntry(entry);
  }
}

function openEntry(entry) {
  if (entry.isDir) {
    void browse(entry.path);
  } else {
    void previewEntry(entry);
  }
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
    selectedEntry.value = entry;
    peerPreview.value = null;
    peerPreviewPath.value = "";
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

async function revealSavedFile() {
  if (!savedPath.value) return;
  try {
    await app.invoke("reveal_local_file", { path: savedPath.value });
    savedOpen.value = false;
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
  const candidates = await app.invoke("collect_upload_paths", { paths });
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
  return app.invoke("upload_peer_file", {
    peerId: props.peer.id,
    shareId: activeShareId.value,
    localPath: item.localPath,
    remoteDir,
    conflict,
  });
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
      if (normalized.code !== "TRANSFER_CONFLICT") {
        item.status = "error";
        item.error = normalized.message;
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
        item.status = "error";
        item.error = normalizeAppError(retryError).message;
        markRemainingCanceled();
        app.handleFailure(retryError);
        break;
      }
    }
  }
  uploadRunning.value = false;
  await loadPath(peerPath.value);
  if (!uploadCanceled.value && completed > 0) {
    app.setNotice("peer.uploadFinished", { count: completed });
  }
}

function cancelUploadQueue() {
  if (!uploadRunning.value || uploadCanceled.value) return;
  uploadCanceled.value = true;
  markRemainingCanceled();
}

function uploadStatusText(item) {
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

async function previewEntry(entry) {
  if (!entry || entry.isDir || busy.value) return;
  await run(async () => {
    const preview = await app.invoke("preview_peer_file", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
    selectedEntry.value = entry;
    peerPreview.value = preview;
    peerPreviewPath.value = entry.path;
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
          <div class="breadcrumbs" aria-label="Breadcrumb">
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
            <div class="view-switch" role="group" :aria-label="t('peer.viewMode')">
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
        </div>

        <p v-if="peerNotice" class="browser-notice">{{ peerNotice }}</p>

        <div v-if="peerListing" class="file-view">
          <div v-if="viewMode === 'icons'" class="icon-grid">
            <button
              v-for="entry in peerListing.entries"
              :key="entry.path"
              type="button"
              class="file-tile"
              :class="{ 'is-selected': selectedEntry?.path === entry.path }"
              @click="selectEntry(entry)"
              @dblclick="openEntry(entry)"
            >
              <FileGlyph :entry="entry" size="lg" />
              <span class="file-tile__name">{{ entry.name }}</span>
              <span class="file-tile__meta">{{ entryKind(entry) }}</span>
            </button>
          </div>

          <table v-else-if="viewMode === 'list'" class="file-table">
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
                :class="{ 'is-selected': selectedEntry?.path === entry.path }"
                @click="selectEntry(entry)"
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

          <div v-else-if="viewMode === 'columns'" class="column-view">
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
                :class="{ 'is-selected': selectedEntry?.path === entry.path }"
                @click="loadColumn(entry, index)"
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

          <div v-else class="gallery-view">
            <div class="gallery-stage">
              <template v-if="galleryFile">
                <img
                  v-if="isImagePreview && peerPreviewSource"
                  :src="peerPreviewSource"
                  :alt="t('peer.previewAlt')"
                  class="gallery-image"
                />
                <pre
                  v-else-if="activePreview && activePreview.encoding === 'utf8'"
                  class="gallery-text"
                >{{ activePreview.data }}</pre>
                <div v-else class="gallery-placeholder">
                  <FileGlyph :entry="galleryFile" size="xl" />
                  <strong>{{ galleryFile.name }}</strong>
                  <span>{{ entryKind(galleryFile) }} · {{ formatSize(galleryFile) }}</span>
                  <el-button type="primary" plain :disabled="busy" @click="previewEntry(galleryFile)">
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
                :class="{ 'is-selected': selectedEntry?.path === entry.path }"
                @click="selectEntry(entry)"
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

        <div v-if="selectedEntry" class="selection-bar">
          <div class="selection-summary">
            <FileGlyph :entry="selectedEntry" />
            <div>
              <strong>{{ selectedEntry.name }}</strong>
              <span>
                {{ entryKind(selectedEntry) }}
                <template v-if="!selectedEntry.isDir">
                  · {{ formatSize(selectedEntry) }}
                </template>
              </span>
            </div>
          </div>
          <div class="selection-actions">
            <el-button
              v-if="selectedEntry.isDir"
              type="primary"
              :disabled="busy"
              @click="browse(selectedEntry.path)"
            >
              {{ t("peer.open") }}
            </el-button>
            <template v-else>
              <el-button
                type="primary"
                plain
                :disabled="busy"
                @click="previewEntry(selectedEntry)"
              >
                {{ t("peer.preview") }}
              </el-button>
              <el-button plain :disabled="busy" @click="downloadEntry(selectedEntry)">
                {{ t("peer.download") }}
              </el-button>
              <el-button plain :disabled="busy" @click="editEntry(selectedEntry)">
                {{ t("peer.edit") }}
              </el-button>
            </template>
          </div>
        </div>

        <section
          v-if="viewMode !== 'gallery' && activePreview"
          class="preview-panel"
        >
          <header class="preview-panel__head">
            <div>
              <strong>{{ activePreview.fileName }}</strong>
              <span>{{ activePreview.mime }} · {{ activePreview.size }} B</span>
            </div>
            <el-button text @click="peerPreview = null">
              {{ t("common.close") }}
            </el-button>
          </header>
          <img
            v-if="isImagePreview && peerPreviewSource"
            :src="peerPreviewSource"
            :alt="t('peer.previewAlt')"
            class="preview-image"
          />
          <pre
            v-else-if="activePreview.encoding === 'utf8'"
            class="preview-text"
          >{{ activePreview.data }}</pre>
          <p v-else class="hint">{{ t("peer.binaryPreview") }}</p>
          <p v-if="activePreview.truncated" class="hint">
            {{ t("peer.previewTruncated") }}
          </p>
        </section>

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
        <ul class="upload-queue">
          <li
            v-for="(item, index) in uploadQueue"
            :key="`${item.localPath}-${index}`"
          >
            <div class="upload-queue__name">
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
            <span
              class="upload-queue__status"
              :class="`is-${item.status}`"
              :title="item.error || undefined"
            >
              {{ uploadStatusText(item) }}
            </span>
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
  </div>
</template>

<style scoped>
.peer-browser {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: 100%;
  min-height: 540px;
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
  padding: 3px;
  border: 1px solid var(--app-border);
  border-radius: 9px;
  background: var(--app-surface-2);
}

.view-switch__button {
  width: 30px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--app-text-muted);
  cursor: pointer;
}

.view-switch__button:hover {
  color: var(--app-text);
  background: var(--app-surface-3);
}

.view-switch__button.is-active {
  color: var(--el-color-primary);
  background: var(--app-surface);
  box-shadow: 0 1px 4px rgba(22, 32, 46, 0.1);
}

.view-switch__button svg {
  width: 16px;
  height: 16px;
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
  border: 1px solid var(--app-border);
  border-radius: 16px;
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
  font-weight: 650;
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
  background: var(--app-surface);
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
  overflow-y: auto;
  padding: 14px 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pathbar {
  display: flex;
  align-items: center;
  gap: 12px;
}

.breadcrumbs {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 2px;
  overflow-x: auto;
  white-space: nowrap;
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
  font-weight: 650;
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
  gap: 4px;
}

.browser-notice {
  margin: 0;
  padding: 8px 10px;
  border-radius: 8px;
  background: var(--app-surface-2);
  color: var(--app-text-secondary);
  font-size: 12.5px;
}

.file-view {
  flex: 1;
  min-height: 280px;
  overflow-x: auto;
}

.icon-grid {
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
  border-color: color-mix(in oklab, var(--el-color-primary) 40%, var(--app-border));
  background: color-mix(in oklab, var(--el-color-primary) 8%, var(--app-surface));
}

.file-tile__name {
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12.5px;
  font-weight: 550;
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
  padding: 7px 9px;
  border-bottom: 1px solid var(--app-border);
  color: var(--app-text-muted);
  font-size: 11.5px;
  font-weight: 600;
  text-align: left;
}

.file-table td {
  padding: 7px 9px;
  border-bottom: 1px solid color-mix(in oklab, var(--app-border) 70%, transparent);
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
  background: color-mix(in oklab, var(--el-color-primary) 8%, var(--app-surface));
}

.table-name {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--app-text);
}

.column-view {
  display: flex;
  align-items: stretch;
  min-height: 320px;
  overflow-x: auto;
  border: 1px solid var(--app-border);
  border-radius: 10px;
}

.column {
  width: 240px;
  flex: none;
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
  background: color-mix(in oklab, var(--el-color-primary) 10%, var(--app-surface-2));
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

.gallery-view {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.gallery-stage {
  min-height: 300px;
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

.gallery-image {
  max-width: 100%;
  max-height: 380px;
  object-fit: contain;
  border-radius: 8px;
  box-shadow: 0 8px 28px rgba(22, 32, 46, 0.16);
}

.gallery-text {
  margin: 0;
  width: 100%;
  max-height: 380px;
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
  border-color: color-mix(in oklab, var(--el-color-primary) 35%, var(--app-border));
  background: color-mix(in oklab, var(--el-color-primary) 8%, var(--app-surface));
}

.file-empty {
  margin: 22px 0;
  text-align: center;
  font-size: 13px;
  color: var(--app-text-muted);
}

.selection-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-2);
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

.preview-panel,
.edit-panel {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 12px;
  background: var(--app-surface-2);
}

.preview-panel__head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 10px;
}

.preview-panel__head div {
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.preview-panel__head strong {
  font-size: 13px;
  color: var(--app-text);
}

.preview-panel__head span {
  font-size: 11.5px;
  color: var(--app-text-muted);
}

.edit-panel {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
}

.edit-panel p {
  margin: 4px 0 0;
}

.transfer-dialog {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

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

.upload-queue {
  list-style: none;
  max-height: 320px;
  overflow-y: auto;
  margin: 0;
  padding: 0;
  border: 1px solid var(--app-border);
  border-radius: 10px;
}

.upload-queue li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 9px 11px;
}

.upload-queue li + li {
  border-top: 1px solid var(--app-border);
}

.upload-queue__name {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 9px;
}

.upload-queue__name > div {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.upload-queue__name strong,
.upload-queue__name span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.upload-queue__name strong {
  font-size: 12.5px;
  color: var(--app-text);
}

.upload-queue__name span {
  font-size: 11px;
  color: var(--app-text-muted);
}

.upload-queue__status {
  flex: none;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--app-text-muted);
}

.upload-queue__status.is-done {
  color: color-mix(in oklab, var(--accent-green) 72%, var(--app-text));
}

.upload-queue__status.is-error {
  color: var(--el-color-danger);
}

.upload-queue__status.is-skipped,
.upload-queue__status.is-canceled {
  color: var(--app-text-muted);
}

.upload-queue__status.is-uploading {
  color: var(--el-color-primary);
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

  .selection-actions,
  .pathbar-actions {
    flex-wrap: wrap;
  }

  .pathbar {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
