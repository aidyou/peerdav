<script setup>
import { computed, ref, watch } from "vue";

import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";
import { useAction } from "../composables/useAction";

const props = defineProps({
  peer: { type: Object, required: true },
});

const emit = defineEmits(["close"]);

const app = useAppContext();
const { busy, run } = useAction();

const peerShares = ref([]);
const activeShareId = ref("");
const peerPath = ref("/");
const peerListing = ref(null);
const peerLocalDir = ref("");
const peerUploadPath = ref("");
const peerUploadDir = ref("/");
const peerConflict = ref("fail");
const peerPreview = ref(null);
const peerEdit = ref(null);
const peerNoticeState = ref(null);

const peerNotice = computed(() => {
  if (!peerNoticeState.value) return "";
  return t(peerNoticeState.value.key, peerNoticeState.value.params);
});

const peerParentPath = computed(() => {
  if (!peerPath.value || peerPath.value === "/") return "/";
  const index = peerPath.value.lastIndexOf("/");
  return index <= 0 ? "/" : peerPath.value.slice(0, index);
});

const peerPreviewSource = computed(() => {
  if (!peerPreview.value || peerPreview.value.encoding !== "base64") return "";
  return `data:${peerPreview.value.mime};base64,${peerPreview.value.data}`;
});

function setPeerNotice(key, params) {
  peerNoticeState.value = { key, params };
}

function reset() {
  peerShares.value = [];
  activeShareId.value = "";
  peerPath.value = "/";
  peerListing.value = null;
  peerPreview.value = null;
  peerEdit.value = null;
  peerNoticeState.value = null;
}

async function loadPeer() {
  reset();
  await run(async () => {
    peerShares.value = await app.invoke("list_peer_shares", {
      peerId: props.peer.id,
    });
    if (peerShares.value.length) {
      await openShare(peerShares.value[0].id);
    }
  });
}

async function openShare(shareId) {
  activeShareId.value = shareId;
  peerPath.value = "/";
  await browse("/");
}

async function browse(path) {
  if (!props.peer.id || !activeShareId.value) return;
  await run(async () => {
    const listing = await app.invoke("list_peer_dir", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      path,
    });
    peerListing.value = listing;
    peerPath.value = listing.path;
  });
}

function enterEntry(entry) {
  if (entry.isDir) {
    void browse(entry.path);
  }
}

async function downloadEntry(entry) {
  if (!peerLocalDir.value.trim()) {
    app.setError("peer.downloadNeedDir");
    return;
  }
  await run(async () => {
    const result = await app.invoke("download_peer_file", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      remotePath: entry.path,
      localDir: peerLocalDir.value,
      conflict: peerConflict.value,
    });
    setPeerNotice("peer.downloaded", { path: result.localPath });
  });
}

async function uploadPeerFile() {
  if (!peerUploadPath.value.trim()) {
    app.setError("peer.uploadNeedPath");
    return;
  }
  await run(async () => {
    const result = await app.invoke("upload_peer_file", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      localPath: peerUploadPath.value,
      remoteDir: peerUploadDir.value || peerPath.value,
      conflict: peerConflict.value,
    });
    setPeerNotice("peer.uploaded", { path: result.remotePath });
    await browse(peerPath.value);
  });
}

async function previewEntry(entry) {
  await run(async () => {
    peerPreview.value = await app.invoke("preview_peer_file", {
      peerId: props.peer.id,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
  });
}

async function editEntry(entry) {
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
  if (!peerEdit.value) return;
  await run(async () => {
    const result = await app.invoke("commit_peer_external_edit", {
      editId: peerEdit.value.editId,
      conflict: peerConflict.value,
    });
    if (result.transferred) {
      setPeerNotice("peer.editUploaded", { path: result.remotePath });
    } else {
      setPeerNotice("peer.editNoChange");
    }
    peerEdit.value = null;
    await browse(peerPath.value);
  });
}

async function discardPeerEdit() {
  if (!peerEdit.value) return;
  await run(async () => {
    await app.invoke("discard_peer_external_edit", {
      editId: peerEdit.value.editId,
    });
    peerEdit.value = null;
    setPeerNotice("peer.editDiscarded");
  });
}

watch(() => props.peer.id, loadPeer, { immediate: true });
</script>

<template>
  <div class="peer-browser">
    <header class="peer-browser__head">
      <el-button plain @click="emit('close')">
        ← {{ t("home.title") }}
      </el-button>
      <div class="peer-browser__identity">
        <strong>{{ peer.label }}</strong>
        <span class="hint mono">{{ peer.url }}</span>
      </div>
      <el-button plain :disabled="busy" @click="loadPeer">
        {{ t("client.refreshSaved") }}
      </el-button>
    </header>

    <p v-if="peerNotice" class="hint">{{ peerNotice }}</p>

    <div class="actions">
      <el-button
        v-for="share in peerShares"
        :key="share.id"
        plain
        :disabled="busy"
        :type="share.id === activeShareId ? 'primary' : ''"
        @click="openShare(share.id)"
      >
        {{ share.name }}
      </el-button>
      <span v-if="!peerShares.length" class="hint">{{ t("peer.emptyDir") }}</span>
    </div>

    <div v-if="peerListing">
      <p class="hint mono">{{ t("peer.path", { path: peerListing.path }) }}</p>
      <div class="actions">
        <el-button plain :disabled="busy" @click="browse(peerParentPath)">
          {{ t("peer.up") }}
        </el-button>
        <el-button plain :disabled="busy" @click="browse('/')">
          {{ t("peer.root") }}
        </el-button>
      </div>
      <ul class="list">
        <li v-for="entry in peerListing.entries" :key="entry.path">
          <div>
            <strong>{{ entry.name }}</strong>
            <div class="hint">
              {{
                entry.isDir
                  ? t("peer.directory")
                  : t("peer.fileMeta", {
                      size: entry.size,
                      contentType: entry.contentType || t("peer.unknownType"),
                    })
              }}
            </div>
            <div class="hint mono">{{ entry.path }}</div>
          </div>
          <div class="actions">
            <el-button
              v-if="entry.isDir"
              type="primary"
              :disabled="busy"
              @click="enterEntry(entry)"
            >
              {{ t("peer.open") }}
            </el-button>
            <template v-else>
              <el-button
                type="primary"
                :disabled="busy"
                @click="previewEntry(entry)"
              >
                {{ t("peer.preview") }}
              </el-button>
              <el-button plain :disabled="busy" @click="downloadEntry(entry)">
                {{ t("peer.download") }}
              </el-button>
              <el-button plain :disabled="busy" @click="editEntry(entry)">
                {{ t("peer.edit") }}
              </el-button>
            </template>
          </div>
        </li>
        <li v-if="!peerListing.entries.length" class="hint">
          {{ t("peer.emptyDir") }}
        </li>
      </ul>
      <p v-if="peerListing.truncated" class="hint">
        {{ t("peer.truncated") }}
      </p>

      <div class="grid">
        <label>
          {{ t("peer.localDownloadDir") }}
          <el-input
            v-model="peerLocalDir"
            type="text"
            placeholder="/home/user/Downloads"
          />
        </label>
        <label>
          {{ t("peer.localFile") }}
          <el-input
            v-model="peerUploadPath"
            type="text"
            placeholder="/home/user/a.txt"
          />
        </label>
        <label>
          {{ t("peer.remoteDir") }}
          <el-input v-model="peerUploadDir" type="text" placeholder="/" />
        </label>
        <label>
          {{ t("peer.conflict") }}
          <el-select v-model="peerConflict">
            <el-option value="fail" :label="t('peer.conflictFail')" />
            <el-option value="overwrite" :label="t('peer.conflictOverwrite')" />
            <el-option value="rename" :label="t('peer.conflictRename')" />
          </el-select>
        </label>
      </div>
      <div class="actions">
        <el-button type="primary" :disabled="busy" @click="uploadPeerFile">
          {{ t("peer.upload") }}
        </el-button>
      </div>
    </div>

    <div v-if="peerPreview" class="code-box">
      <p class="hint">
        {{ peerPreview.fileName }} · {{ peerPreview.mime }} ·
        {{ peerPreview.size }} B
      </p>
      <img
        v-if="peerPreviewSource"
        :src="peerPreviewSource"
        :alt="t('peer.previewAlt')"
        class="preview-image"
      />
      <pre
        v-else-if="peerPreview.encoding === 'utf8'"
        class="preview-text"
      >{{ peerPreview.data }}</pre>
      <p v-else class="hint">{{ t("peer.binaryPreview") }}</p>
      <p v-if="peerPreview.truncated" class="hint">
        {{ t("peer.previewTruncated") }}
      </p>
      <el-button plain @click="peerPreview = null">
        {{ t("common.close") }}
      </el-button>
    </div>

    <div v-if="peerEdit" class="code-box">
      <p class="hint">
        {{ t("peer.externalEdit", { path: peerEdit.remotePath }) }}
      </p>
      <p class="hint mono">{{ peerEdit.localPath }}</p>
      <div class="actions">
        <el-button type="primary" :disabled="busy" @click="commitPeerEdit">
          {{ t("peer.uploadChanges") }}
        </el-button>
        <el-button plain :disabled="busy" @click="discardPeerEdit">
          {{ t("peer.discard") }}
        </el-button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.peer-browser {
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 760px;
  margin: 0 auto;
}

.peer-browser__head {
  display: flex;
  align-items: center;
  gap: 14px;
}

.peer-browser__identity {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.peer-browser__identity .mono {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>