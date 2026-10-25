<script setup>
import { computed, onMounted, ref } from "vue";

import { confirmAction } from "../lib/confirm";
import { normalizeAppError } from "../lib/invoke";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const app = useAppContext();

const discovered = ref([]);
const scanState = ref("idle");
const scanError = ref("");
const activeRequest = ref(null);
const canceling = ref(false);
const cancelRequested = ref(false);
const removingPeerId = ref("");

const hasPeers = computed(() => app.peers.length > 0);
const requestBusy = computed(() =>
  Boolean(
    activeRequest.value &&
      ["requesting", "waiting"].includes(activeRequest.value.state),
  ),
);

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

function isSaved(server) {
  return app.peers.some((peer) => peer.url === server.url);
}

function requestFor(server) {
  return activeRequest.value?.url === server.url ? activeRequest.value : null;
}

function requestMessage(server) {
  const request = requestFor(server);
  if (!request) return "";
  if (request.state === "requesting") return t("home.requesting");
  if (request.state === "waiting") return t("home.addWaiting");
  if (request.state === "accessible") return t("home.addAlreadyAccessible");
  if (request.state === "canceled") return t("home.requestCanceled");
  return "";
}

function requestError(server) {
  return requestFor(server)?.error ?? "";
}

function canRequest(server) {
  if (isSaved(server)) return false;
  const request = requestFor(server);
  return !request || ["accessible", "canceled", "error"].includes(request.state);
}

function canCancel(server) {
  const request = requestFor(server);
  return request?.state === "waiting" && Boolean(request.requestId);
}

async function beginAdd(server) {
  if (requestBusy.value) {
    app.setNotice("home.requestInProgress");
    return;
  }
  if (isSaved(server)) {
    const peer = app.peers.find((item) => item.url === server.url);
    if (peer) app.openPeer(peer);
    return;
  }

  activeRequest.value = {
    url: server.url,
    label: server.name,
    serverId: server.serverId,
    requestId: "",
    state: "requesting",
    error: "",
  };
  cancelRequested.value = false;

  try {
    const request = await app.invoke("request_peer_access", {
      url: server.url,
    });
    if (!request.requested) {
      activeRequest.value.state = "accessible";
      return;
    }
    if (!request.requestId) {
      activeRequest.value.state = "error";
      activeRequest.value.error = t("home.addRequestMissing");
      return;
    }

    activeRequest.value.requestId = request.requestId;
    activeRequest.value.state = "waiting";
    const peer = await app.invoke("wait_for_peer_access", {
      url: server.url,
      requestId: request.requestId,
      label: server.name,
    });
    await app.refreshPeers();
    activeRequest.value = null;
    app.setNotice("home.added", { label: peer.label });
  } catch (error) {
    const normalized = normalizeAppError(error);
    const canceled =
      cancelRequested.value || normalized.code === "PEER_ACCESS_CANCELED";
    if (activeRequest.value) {
      activeRequest.value.requestId = "";
      if (canceled) {
        activeRequest.value.state = "canceled";
        activeRequest.value.error = "";
        app.setNotice("home.requestCanceled");
      } else {
        activeRequest.value.state = "error";
        activeRequest.value.error = messageForError(error, "request peer access");
      }
    }
  } finally {
    cancelRequested.value = false;
    canceling.value = false;
  }
}

async function cancelRequest(server) {
  const request = requestFor(server);
  if (!request?.requestId || canceling.value) return;
  canceling.value = true;
  cancelRequested.value = true;
  try {
    await app.invoke("cancel_peer_access", {
      url: request.url,
      requestId: request.requestId,
    });
  } catch (error) {
    cancelRequested.value = false;
    request.error = messageForError(error, "cancel peer access");
    canceling.value = false;
  }
}

async function forgetPeer(peer) {
  if (removingPeerId.value === peer.id) return;
  removingPeerId.value = peer.id;
  try {
    const confirmed = await confirmAction(
      t("home.forgetConfirm", { label: peer.label }),
      t("home.forget"),
    );
    if (!confirmed) return;

    await app.invoke("forget_peer", { peerId: peer.id });
    await app.refreshPeers();
    app.setNotice("home.forgotten", { label: peer.label });
  } catch (error) {
    app.handleFailure(error);
  } finally {
    removingPeerId.value = "";
  }
}
</script>

<template>
  <div class="home-view">
    <section class="home-hero">
      <div class="home-hero__copy">
        <p class="home-eyebrow">{{ t("home.eyebrow") }}</p>
        <h1>{{ t("home.title") }}</h1>
        <p class="home-subtitle">{{ t("home.subtitle") }}</p>
      </div>
      <div class="home-hero__aside">
        <span class="home-status" :class="{ 'is-empty': !hasPeers }">
          <span class="home-status__dot" aria-hidden="true"></span>
          {{
            hasPeers
              ? t("home.connectedCount", { count: app.peers.length })
              : t("home.notConnected")
          }}
        </span>
        <el-button
          plain
          :loading="scanState === 'scanning'"
          @click="scanLan"
        >
          {{ scanState === "scanning" ? t("home.scanning") : t("home.scan") }}
        </el-button>
      </div>
    </section>

    <section v-if="hasPeers" class="home-section">
      <header class="section-heading">
        <div>
          <h2>{{ t("home.authorizedServers") }}</h2>
          <p class="section-hint">{{ t("home.connectedHint") }}</p>
        </div>
        <span class="section-count">{{ app.peers.length }}</span>
      </header>

      <ul class="server-list">
        <li v-for="peer in app.peers" :key="peer.id" class="server-item">
          <button
            type="button"
            class="server-card server-item__main"
            :aria-label="t('home.openServer', { label: peer.label })"
            @click="app.openPeer(peer)"
          >
            <span class="server-item__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <rect x="3.5" y="4.5" width="17" height="11" rx="2" />
                <path d="M9 19h6M12 15.5V19" />
              </svg>
            </span>
            <span class="server-item__body">
              <strong>{{ peer.label }}</strong>
              <span class="server-item__url">{{ peer.url }}</span>
            </span>
            <span class="server-item__meta">
              {{ t("home.shareCount", { count: peer.shares.length }) }}
            </span>
            <svg class="server-item__arrow" viewBox="0 0 24 24" aria-hidden="true">
              <path d="m9 6 6 6-6 6" />
            </svg>
          </button>
          <div class="server-item__actions">
            <el-button
              text
              size="small"
              :disabled="removingPeerId === peer.id"
              @click.stop="forgetPeer(peer)"
            >
              {{ t("home.forget") }}
            </el-button>
          </div>
        </li>
      </ul>
    </section>

    <section class="home-section home-section--discovery">
      <header class="section-heading">
        <div>
          <h2>{{ t("home.discoveredServers") }}</h2>
          <p class="section-hint">{{ t("home.discoveryHint") }}</p>
        </div>
      </header>

      <p v-if="scanError" class="home-error">{{ scanError }}</p>

      <ul v-if="discovered.length" class="server-list">
        <li v-for="server in discovered" :key="server.serverId" class="server-item">
          <div class="server-item__main server-item__main--static">
            <span class="server-item__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <rect x="4" y="4" width="16" height="6" rx="1.5" />
                <rect x="4" y="14" width="16" height="6" rx="1.5" />
                <path d="M8 7h.01M8 17h.01" />
              </svg>
            </span>
            <span class="server-item__body">
              <strong>{{ server.name }}</strong>
              <span class="server-item__url">{{ server.url }}</span>
              <span class="server-item__detail">
                {{ t("home.discoveredMeta", {
                  count: server.shareCount,
                  hosts: server.hosts.join(", "),
                }) }}
              </span>
            </span>
            <span v-if="isSaved(server)" class="server-state is-connected">
              {{ t("home.connected") }}
            </span>
            <div v-else class="server-request">
              <el-button
                v-if="canRequest(server)"
                type="primary"
                plain
                :disabled="requestBusy"
                @click="beginAdd(server)"
              >
                {{ t("home.add") }}
              </el-button>
              <el-button
                v-if="canCancel(server)"
                plain
                :loading="canceling"
                @click="cancelRequest(server)"
              >
                {{ canceling ? t("home.canceling") : t("home.requestCancel") }}
              </el-button>
            </div>
          </div>
          <p
            v-if="requestMessage(server) || requestError(server)"
            class="request-status"
            :class="{ 'is-error': requestError(server) }"
          >
            {{ requestError(server) || requestMessage(server) }}
          </p>
        </li>
      </ul>

      <div
        v-else-if="scanState === 'done' && !scanError"
        class="home-empty"
      >
        <span class="home-empty__icon" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <circle cx="12" cy="12" r="8" />
            <path d="M12 8v4M12 15h.01" />
          </svg>
        </span>
        <div>
          <strong>{{ t("home.scanEmptyTitle") }}</strong>
          <p>{{ t("home.scanEmpty") }}</p>
        </div>
      </div>

      <p v-else-if="scanState === 'scanning'" class="hint">
        {{ t("home.discoveryHint") }}
      </p>
    </section>
  </div>
</template>

<style scoped>
.home-view {
  max-width: 860px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.home-hero {
  position: relative;
  overflow: hidden;
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 28px;
  padding: 30px 30px 28px;
  border: 1px solid var(--app-border);
  border-radius: 22px;
  background:
    radial-gradient(
      circle at 92% 12%,
      color-mix(in oklab, var(--accent) 18%, transparent),
      transparent 42%
    ),
    linear-gradient(145deg, var(--app-surface), var(--app-surface-2));
  box-shadow: 0 18px 44px -32px rgba(22, 32, 46, 0.4);
}

.home-eyebrow {
  margin: 0 0 10px;
  font-size: 12px;
  font-weight: 650;
  letter-spacing: 0.02em;
  color: var(--el-color-primary);
}

.home-hero h1 {
  margin: 0;
  font-size: clamp(28px, 4vw, 36px);
  line-height: 1.05;
  letter-spacing: -0.035em;
  color: var(--app-text);
}

.home-subtitle {
  max-width: 46ch;
  margin: 12px 0 0;
  font-size: 14px;
  line-height: 1.65;
  color: var(--app-text-secondary);
}

.home-hero__aside {
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 12px;
}

.home-status {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 7px 11px;
  border: 1px solid color-mix(in oklab, var(--accent-green) 35%, var(--app-border));
  border-radius: 999px;
  background: color-mix(in oklab, var(--accent-green) 8%, var(--app-surface));
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text-secondary);
}

.home-status.is-empty {
  border-color: var(--app-border);
  background: var(--app-surface-3);
}

.home-status__dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent-green);
  box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent-green) 16%, transparent);
}

.home-status.is-empty .home-status__dot {
  background: var(--app-text-muted);
  box-shadow: none;
}

.home-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.home-section--discovery {
  padding-top: 4px;
}

.section-heading {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 20px;
}

.section-heading h2 {
  margin: 0;
  font-size: 17px;
  letter-spacing: -0.01em;
  color: var(--app-text);
}

.section-hint {
  margin: 4px 0 0;
  font-size: 13px;
  line-height: 1.5;
  color: var(--app-text-muted);
}

.section-count {
  display: inline-flex;
  min-width: 26px;
  height: 26px;
  align-items: center;
  justify-content: center;
  border-radius: 999px;
  background: var(--app-surface-3);
  border: 1px solid var(--app-border);
  font-size: 12px;
  font-weight: 650;
  color: var(--app-text-muted);
}

.server-list {
  list-style: none;
  margin: 0;
  padding: 0;
  overflow: hidden;
  border: 1px solid var(--app-border);
  border-radius: 16px;
  background: var(--app-surface);
}

.server-item {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px 6px 8px;
}

.server-item + .server-item {
  border-top: 1px solid var(--app-border);
}

.server-item:hover {
  background: var(--app-surface-2);
}

.server-item__main {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 8px;
  border: 0;
  border-radius: 10px;
  background: transparent;
  color: var(--app-text);
  text-align: left;
  font: inherit;
  cursor: pointer;
}

.server-item__main--static {
  cursor: default;
}

.server-item__icon {
  width: 46px;
  height: 46px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 13px;
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 9%, var(--app-surface-2));
  border: 1px solid color-mix(in oklab, var(--el-color-primary) 14%, var(--app-border));
}

.server-item__icon svg {
  width: 24px;
  height: 24px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.server-item__body {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.server-item__body strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 14px;
  font-weight: 650;
  color: var(--app-text);
}

.server-item__url,
.server-item__detail {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 11.5px;
  color: var(--app-text-muted);
}

.server-item__detail {
  font-family: inherit;
  font-size: 12px;
}

.server-item__meta {
  flex: none;
  font-size: 12px;
  color: var(--app-text-secondary);
}

.server-item__arrow {
  width: 18px;
  height: 18px;
  flex: none;
  fill: none;
  stroke: var(--app-text-muted);
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.server-item__actions {
  flex: none;
}

.server-request {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
}

.server-state {
  flex: none;
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text-muted);
}

.server-state.is-connected {
  color: color-mix(in oklab, var(--accent-green) 72%, var(--app-text));
}

.request-status {
  margin: -2px 12px 10px 68px;
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

.request-status.is-error {
  color: var(--el-color-danger);
}

.home-empty {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 22px;
  border: 1px dashed var(--app-border);
  border-radius: 16px;
  background: color-mix(in oklab, var(--app-surface-2) 70%, transparent);
}

.home-empty__icon {
  width: 40px;
  height: 40px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  background: var(--app-surface-3);
  color: var(--app-text-muted);
}

.home-empty__icon svg {
  width: 22px;
  height: 22px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.home-empty strong {
  display: block;
  margin-bottom: 4px;
  font-size: 14px;
  color: var(--app-text);
}

.home-empty p {
  margin: 0;
  font-size: 13px;
  line-height: 1.55;
  color: var(--app-text-muted);
}

.home-error {
  margin: 0;
  color: var(--el-color-danger);
  font-size: 13px;
}

@media (max-width: 640px) {
  .home-hero {
    align-items: flex-start;
    flex-direction: column;
    padding: 24px;
  }

  .home-hero__aside {
    width: 100%;
    align-items: stretch;
    flex-direction: row;
    justify-content: space-between;
  }

  .server-item {
    padding-right: 8px;
  }

  .server-item__meta {
    display: none;
  }

  .request-status {
    margin-left: 12px;
  }
}
</style>