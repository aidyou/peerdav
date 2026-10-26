<script setup>
import { computed, ref, watch } from "vue";

import { confirmAction } from "../lib/confirm";
import { normalizeAppError } from "../lib/invoke";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";

const app = useAppContext();

const discovered = computed(() => app.discovered);
const activeRequest = ref(null);
const canceling = ref(false);
const cancelRequested = ref(false);
const removingPeerId = ref("");
const shareCounts = ref({});

const hasPeers = computed(() => app.peers.length > 0);
const requestBusy = computed(() =>
  Boolean(
    activeRequest.value &&
      ["requesting", "waiting"].includes(activeRequest.value.state),
  ),
);

/// The saved-peer list can arrive after this view mounts, so the live share
/// counts are driven by a watcher rather than a one-shot mount check.
watch(
  () =>
    app.peers
      .map((peer) => `${peer.id}:${peer.online ? "1" : "0"}`)
      .join("|"),
  (key) => {
    if (key) void refreshShareCounts();
  },
  { immediate: true },
);

/// Saved peers carry no share list in the local record, so the home cards read
/// each online peer's live catalog to show a real count. Offline or
/// unauthorized peers fall back to a dash rather than a misleading zero.
async function refreshShareCounts() {
  const results = await Promise.all(
    app.peers.map(async (peer) => {
      if (!peer.online) return [peer.id, null];
      try {
        const shares = await app.invoke("list_peer_shares", { peerId: peer.id });
        return [peer.id, shares.length];
      } catch (error) {
        console.warn(`PeerDav share count unavailable for ${peer.id}`, error);
        return [peer.id, null];
      }
    }),
  );
  shareCounts.value = Object.fromEntries(results);
}

function peerCount(peer) {
  const count = shareCounts.value[peer.id];
  return typeof count === "number" ? count : null;
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
    <section class="home-section">
      <header class="section-heading">
        <h2>{{ t("home.authorizedServers") }}</h2>
        <span v-if="hasPeers" class="section-summary">
          {{ t("home.totalServers", { count: app.peers.length }) }}
        </span>
      </header>

      <ul v-if="hasPeers" class="server-list">
        <li v-for="peer in app.peers" :key="peer.id" class="server-row">
          <div
            class="server-row__line"
            :class="{
              'is-clickable': peer.online,
              'is-offline': !peer.online,
            }"
            @click="peer.online && app.openPeer(peer)"
          >
            <button
              type="button"
              class="server-row__main"
              :disabled="!peer.online"
              :aria-label="t('home.openServer', { label: peer.label })"
              @click.stop="peer.online && app.openPeer(peer)"
            >
              <span class="server-row__icon" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <rect x="3.5" y="4.5" width="17" height="11" rx="2" />
                  <path d="M9 19h6M12 15.5V19" />
                </svg>
              </span>
              <span class="server-row__title">
                <strong>{{ peer.label }}</strong>
                <span class="server-row__url">{{ peer.url }}</span>
              </span>
            </button>
            <span class="server-row__count">
              <strong>{{ peerCount(peer) ?? "—" }}</strong>
              <em v-if="peerCount(peer) !== null">
                {{ t("home.sharesUnit") }}
              </em>
            </span>
            <span v-if="!peer.online" class="server-row__badge is-offline">
              {{ t("home.offline") }}
            </span>
            <button
              type="button"
              class="server-row__badge"
              :disabled="removingPeerId === peer.id"
              @click.stop="forgetPeer(peer)"
            >
              {{ t("home.forget") }}
            </button>
            <span class="server-row__chevron" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </span>
          </div>
        </li>
      </ul>
      <div v-else class="home-empty">
        <span class="home-empty__icon" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <rect x="3.5" y="4.5" width="17" height="11" rx="2" />
            <path d="M9 19h6M12 15.5V19" />
          </svg>
        </span>
        <div>
          <strong>{{ t("home.noPeersTitle") }}</strong>
          <p>{{ t("home.noPeersHint") }}</p>
        </div>
      </div>
    </section>

    <section class="home-section">
      <header class="section-heading">
        <h2>{{ t("home.discoveredServers") }}</h2>
        <span class="section-summary">
          {{ t("home.discoveryListening") }}
        </span>
      </header>

      <ul v-if="discovered.length" class="server-list">
        <li
          v-for="server in discovered"
          :key="server.serverId"
          class="server-row"
        >
          <div class="server-row__line">
            <button
              type="button"
              class="server-row__main"
              :disabled="!isSaved(server) && !canRequest(server)"
              :aria-label="
                isSaved(server)
                  ? t('home.openServer', { label: server.name })
                  : t('home.add')
              "
              @click="beginAdd(server)"
            >
              <span class="server-row__icon" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <rect x="4" y="4" width="16" height="6" rx="1.5" />
                  <rect x="4" y="14" width="16" height="6" rx="1.5" />
                  <path d="M8 7h.01M8 17h.01" />
                </svg>
              </span>
              <span class="server-row__title">
                <strong>{{ server.name }}</strong>
                <span class="server-row__url">{{ server.url }}</span>
              </span>
            </button>
            <span class="server-row__count">
              <strong>{{ server.shareCount }}</strong>
              <em>{{ t("home.sharesUnit") }}</em>
            </span>
            <span
              v-if="isSaved(server)"
              class="server-row__badge is-connected"
            >
              {{ t("home.connected") }}
            </span>
            <button
              v-else-if="canRequest(server)"
              type="button"
              class="server-row__badge is-primary"
              :disabled="requestBusy"
              @click="beginAdd(server)"
            >
              {{ t("home.add") }}
            </button>
            <button
              v-else-if="canCancel(server)"
              type="button"
              class="server-row__badge"
              :disabled="canceling"
              @click="cancelRequest(server)"
            >
              {{ canceling ? t("home.canceling") : t("home.requestCancel") }}
            </button>
            <span class="server-row__chevron" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </span>
          </div>
          <p
            v-if="requestMessage(server) || requestError(server)"
            class="server-row__status"
            :class="{ 'is-error': requestError(server) }"
          >
            {{ requestError(server) || requestMessage(server) }}
          </p>
        </li>
      </ul>

      <div v-else class="home-empty">
        <span class="home-empty__icon" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <circle cx="12" cy="12" r="8" />
            <path d="M12 8v4M12 15h.01" />
          </svg>
        </span>
        <div>
          <strong>{{ t("home.discoveryEmptyTitle") }}</strong>
          <p>{{ t("home.discoveryEmptyHint") }}</p>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.home-view {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 30px;
}

.home-section {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.section-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.section-heading h2 {
  margin: 0;
  font-size: 16px;
  font-weight: 600;
  letter-spacing: -0.01em;
  color: var(--app-text);
}

.section-summary {
  font-size: 13px;
  font-weight: 500;
  color: var(--app-text-muted);
}

.server-list {
  list-style: none;
  margin: 0;
  padding: 0;
  border: 1px solid var(--app-border);
  border-radius: 14px;
  background: var(--app-surface);
  overflow: hidden;
}

.server-row + .server-row {
  border-top: 1px solid var(--app-border);
}

.server-row__line {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 12px 14px 12px 18px;
  transition: background 0.15s ease;
}

.server-row__line:hover {
  background: var(--app-surface-2);
}

.server-row__line.is-clickable {
  cursor: pointer;
}

.server-row__line.is-offline {
  opacity: 0.72;
}

.server-row__line.is-offline:hover {
  background: transparent;
}

.server-row__line.is-clickable:hover .server-row__chevron {
  opacity: 1;
  transform: translateX(2px);
}

.server-row__main {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--app-text);
  text-align: left;
  font: inherit;
  cursor: pointer;
}

.server-row__main:disabled {
  cursor: default;
}

.server-row__main:focus-visible {
  outline: 2px solid var(--el-color-primary);
  outline-offset: 6px;
  border-radius: 8px;
}

.server-row__icon {
  width: 36px;
  height: 36px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 9%, var(--app-surface-2));
  border: 1px solid color-mix(in oklab, var(--el-color-primary) 14%, var(--app-border));
}

.server-row__icon svg {
  width: 19px;
  height: 19px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.server-row__title {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.server-row__title strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 14px;
  font-weight: 600;
  color: var(--app-text);
}

.server-row__url {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 11.5px;
  color: var(--app-text-muted);
}

.server-row__count {
  flex: none;
  display: inline-flex;
  align-items: baseline;
  gap: 4px;
}

.server-row__count strong {
  font-size: 15px;
  font-weight: 600;
  color: var(--app-text);
  font-variant-numeric: tabular-nums;
}

.server-row__count em {
  font-style: normal;
  font-size: 12px;
  color: var(--app-text-muted);
}

.server-row__badge {
  flex: none;
  display: inline-flex;
  align-items: center;
  height: 26px;
  padding: 0 12px;
  border: 1px solid var(--app-border);
  border-radius: 999px;
  background: var(--app-surface);
  color: var(--app-text-secondary);
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
  transition:
    border-color 0.15s ease,
    color 0.15s ease,
    background 0.15s ease;
}

.server-row__badge:hover:not(:disabled) {
  border-color: color-mix(in oklab, var(--el-color-primary) 40%, var(--app-border));
  color: var(--el-color-primary);
}

.server-row__badge:disabled {
  cursor: default;
  opacity: 0.6;
}

.server-row__badge.is-primary {
  border-color: color-mix(in oklab, var(--el-color-primary) 45%, var(--app-border));
  color: var(--el-color-primary);
}

.server-row__badge.is-connected {
  cursor: default;
  border-color: color-mix(in oklab, var(--accent-green) 40%, var(--app-border));
  background: color-mix(in oklab, var(--accent-green) 10%, var(--app-surface));
  color: color-mix(in oklab, var(--accent-green) 72%, var(--app-text));
}

.server-row__badge.is-offline {
  cursor: default;
  color: var(--app-text-muted);
  background: var(--app-surface-2);
}

.server-row__chevron {
  flex: none;
  display: inline-flex;
  align-items: center;
  color: var(--app-text-muted);
  opacity: 0.45;
  transition:
    opacity 0.15s ease,
    transform 0.15s ease;
}

.server-row__main:not(:disabled):hover ~ .server-row__chevron {
  opacity: 1;
  transform: translateX(2px);
}

.server-row__chevron svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.server-row__status {
  margin: -2px 14px 12px 66px;
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

.server-row__status.is-error {
  color: var(--el-color-danger);
}

.home-empty {
  display: flex;
  align-items: center;
  gap: 16px;
  box-sizing: border-box;
  width: 100%;
  padding: 12px 14px 12px 18px;
  border: 1px solid var(--app-border);
  border-radius: 14px;
  background: var(--app-surface);
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

@media (max-width: 560px) {
  .home-view {
    gap: 24px;
  }

  .server-row__line {
    gap: 12px;
    padding-right: 12px;
  }

  .server-row__count em {
    display: none;
  }
}
</style>
