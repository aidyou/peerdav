<script setup>
import { computed, onMounted, ref } from "vue";

import { normalizeAppError } from "../lib/invoke";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const app = useAppContext();

const discovered = ref([]);
const scanState = ref("idle");
const scanError = ref("");

const addOpen = ref(false);
const addTarget = ref(null);
const addCode = ref("");
const addLabel = ref("");
const addRequestState = ref("idle");
const addError = ref("");
const addBusy = ref(false);

const hasPeers = computed(() => app.peers.length > 0);

onMounted(() => {
  if (!hasPeers.value) {
    void scanLan();
  }
});

async function scanLan() {
  if (scanState.value === "scanning") return;
  scanState.value = "scanning";
  scanError.value = "";
  try {
    discovered.value = await app.invoke("scan_lan");
    scanState.value = "done";
  } catch (error) {
    scanError.value = messageForError(error, "scan LAN for servers");
    scanState.value = "done";
  }
}

function messageForError(error, operation) {
  const normalized = normalizeAppError(error);
  console.error(`[peerdav:${normalized.code}] ${operation} failed`, {
    detail: normalized.detail,
    context: normalized.context,
  });
  return normalized.message;
}

function beginAdd(server) {
  addTarget.value = server;
  addCode.value = "";
  addLabel.value = server.name;
  addRequestState.value = "idle";
  addError.value = "";
  addOpen.value = true;
  void requestAccess();
}

async function requestAccess() {
  if (!addTarget.value || addBusy.value) return;
  addBusy.value = true;
  addError.value = "";
  addRequestState.value = "requesting";
  try {
    const result = await app.invoke("request_peer_access", {
      url: addTarget.value.url,
    });
    addRequestState.value = result.requested ? "requested" : "accessible";
  } catch (error) {
    addRequestState.value = "idle";
    addError.value = messageForError(error, "request peer access");
  } finally {
    addBusy.value = false;
  }
}

async function confirmAdd() {
  if (!addTarget.value) return;
  if (!addCode.value.trim()) {
    addError.value = t("home.addCodeRequired");
    return;
  }

  addBusy.value = true;
  addError.value = "";
  try {
    const peer = await app.invoke("connect_peer", {
      url: addTarget.value.url,
      code: addCode.value.trim(),
      label: addLabel.value.trim() || null,
    });
    await app.refreshPeers();
    addOpen.value = false;
    app.openPeer(peer);
    app.setNotice("home.added", { label: peer.label });
  } catch (error) {
    addError.value = messageForError(error, "add peer");
  } finally {
    addBusy.value = false;
  }
}
</script>

<template>
  <div class="home-view">
    <header class="home-header">
      <div>
        <h1>{{ t("home.title") }}</h1>
        <p class="hint">{{ t("home.subtitle") }}</p>
      </div>
      <el-button
        v-if="hasPeers"
        plain
        :disabled="scanState === 'scanning'"
        @click="scanLan"
      >
        {{ t("home.scan") }}
      </el-button>
    </header>

    <template v-if="hasPeers">
      <h2 class="home-section-title">{{ t("home.authorizedServers") }}</h2>
      <ul class="server-grid">
        <li v-for="peer in app.peers" :key="peer.id">
          <button
            type="button"
            class="server-card"
            :aria-label="t('home.openServer', { label: peer.label })"
            @click="app.openPeer(peer)"
          >
            <span class="server-card__badge">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <rect x="4" y="4" width="16" height="6" rx="1.5" />
                <rect x="4" y="14" width="16" height="6" rx="1.5" />
                <path d="M8 7h.01M8 17h.01" />
              </svg>
            </span>
            <span class="server-card__body">
              <strong>{{ peer.label }}</strong>
              <span class="hint mono">{{ peer.url }}</span>
              <span class="server-card__meta">
                {{ t("home.authorized") }} ·
                {{ t("home.shareCount", { count: peer.shares.length }) }}
              </span>
            </span>
            <svg
              class="server-card__arrow"
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path d="m9 6 6 6-6 6" />
            </svg>
          </button>
        </li>
      </ul>
    </template>

    <template v-else>
      <section class="home-empty">
        <h2 class="home-section-title">{{ t("home.noServers") }}</h2>
        <p class="hint">{{ t("home.discoveryHint") }}</p>

        <div class="home-scan">
          <el-button
            type="primary"
            :loading="scanState === 'scanning'"
            @click="scanLan"
          >
            {{ scanState === "scanning" ? t("home.scanning") : t("home.scan") }}
          </el-button>
        </div>

        <p v-if="scanError" class="home-error">{{ scanError }}</p>

        <template v-if="discovered.length">
          <h3>{{ t("home.discoveredServers") }}</h3>
          <ul class="server-grid">
            <li v-for="server in discovered" :key="server.serverId">
              <article class="server-card server-card--discovered">
                <span class="server-card__badge">
                  <svg viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="4" y="4" width="16" height="6" rx="1.5" />
                    <rect x="4" y="14" width="16" height="6" rx="1.5" />
                    <path d="M8 7h.01M8 17h.01" />
                  </svg>
                </span>
                <span class="server-card__body">
                  <strong>{{ server.name }}</strong>
                  <span class="hint mono">{{ server.url }}</span>
                  <span class="server-card__meta">
                    {{ t("home.shareCount", { count: server.shareCount }) }} ·
                    {{ server.hosts.join(", ") }}
                  </span>
                </span>
                <el-button type="primary" plain @click="beginAdd(server)">
                  {{ t("home.add") }}
                </el-button>
              </article>
            </li>
          </ul>
        </template>

        <p v-else-if="scanState === 'done' && !scanError" class="hint">
          {{ t("home.scanEmpty") }}
        </p>
      </section>
    </template>

    <el-dialog
      v-model="addOpen"
      :title="t('home.addTitle')"
      width="420"
      :close-on-click-modal="false"
    >
      <div v-if="addTarget" class="add-server">
        <p class="mono">{{ addTarget.url }}</p>
        <p v-if="addRequestState === 'requesting'" class="hint">
          {{ t("home.requesting") }}
        </p>
        <p v-else-if="addRequestState === 'requested'" class="hint">
          {{ t("home.addRequested") }}
        </p>
        <p v-else-if="addRequestState === 'accessible'" class="hint">
          {{ t("home.addAlreadyAccessible") }}
        </p>

        <label>
          {{ t("home.addLabel") }}
          <el-input v-model="addLabel" type="text" maxlength="64" />
        </label>
        <label>
          {{ t("home.addCode") }}
          <el-input
            v-model="addCode"
            type="password"
            show-password
            autocomplete="off"
            :placeholder="t('home.addCodePlaceholder')"
          />
        </label>
        <p v-if="addError" class="home-error">{{ addError }}</p>
      </div>
      <template #footer>
        <el-button plain @click="addOpen = false">
          {{ t("home.cancel") }}
        </el-button>
        <el-button
          plain
          :disabled="addBusy"
          @click="requestAccess"
        >
          {{ t("home.requestAccess") }}
        </el-button>
        <el-button
          type="primary"
          :disabled="addBusy || !addCode.trim()"
          @click="confirmAdd"
        >
          {{ t("home.confirmAdd") }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.home-view {
  max-width: 820px;
  margin: 0 auto;
}

.home-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 20px;
  margin-bottom: 18px;
}

.home-header h1 {
  margin: 0 0 6px;
  font-size: 22px;
  line-height: 1.2;
}

.home-header .hint {
  margin: 0;
}

.home-section-title {
  margin: 0 0 10px;
  font-size: 15px;
  color: var(--app-text-strong);
}

.server-grid {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 12px;
}

.server-card {
  width: 100%;
  min-height: 96px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px;
  border: 1px solid var(--app-border);
  border-radius: 12px;
  background: var(--app-surface);
  color: var(--app-text);
  text-align: left;
  font: inherit;
  cursor: pointer;
  transition:
    border-color 0.15s ease,
    background 0.15s ease,
    transform 0.15s ease;
}

button.server-card:hover {
  border-color: var(--el-color-primary);
  background: var(--app-surface-2);
  transform: translateY(-1px);
}

.server-card--discovered {
  cursor: default;
}

.server-card__badge {
  flex: none;
  width: 38px;
  height: 38px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 12%, transparent);
}

.server-card__badge svg,
.server-card__arrow {
  width: 19px;
  height: 19px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.server-card__body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.server-card__body strong,
.server-card__body .mono {
  overflow-wrap: anywhere;
}

.server-card__meta {
  font-size: 12px;
  color: var(--app-text-muted);
}

.server-card__arrow {
  flex: none;
  color: var(--app-text-muted);
}

.home-empty {
  padding: 18px;
  border: 1px dashed var(--app-border);
  border-radius: 14px;
  background: var(--app-surface-3);
}

.home-empty h3 {
  margin: 20px 0 10px;
  font-size: 14px;
  color: var(--app-text-strong);
}

.home-scan {
  margin: 14px 0 4px;
}

.home-error {
  margin: 10px 0;
  color: var(--el-color-danger);
  font-size: 13px;
}

.add-server {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.add-server > p {
  margin: 0;
}

.add-server label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text-secondary);
}

@media (max-width: 560px) {
  .home-header {
    align-items: stretch;
    flex-direction: column;
  }

  .server-grid {
    grid-template-columns: 1fr;
  }
}
</style>