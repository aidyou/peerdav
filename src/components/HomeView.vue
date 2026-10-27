<script setup>
import { computed, ref, watch } from "vue";

import { confirmAction } from "../lib/confirm";
import { findAccessRequest, findSavedPeer, accessRowState } from "../lib/access";
import { pendingIdentity } from "../lib/deviceCode";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";
import { statusTagTone } from "../lib/tagTone";
import ListSkeleton from "./ListSkeleton.vue";

const app = useAppContext();

const discovered = computed(() => app.discovered);
const discoveryRunning = computed(() => app.serverStatus.discoveryRunning);
// 首页是控台：别人发来的访问申请先摆在这里，审批后立即从列表消失。
const pendingApprovals = computed(() => app.pendingRequests ?? []);
const approvingId = ref("");
// 骨架屏只在「还没有任何数据」时出现：已有列表时后台刷新继续显示旧数据，避免闪烁。
const peersLoading = computed(() => app.peersLoading && app.peers.length === 0);
const discoveredLoading = computed(
  () => app.discoveredLoading && discovered.value.length === 0,
);
const requestingUrl = ref("");
const removingPeerId = ref("");
// 共享数量放在共享上下文里：页面重挂载时不会先掉成「—」再填回来。
const shareCounts = computed(() => app.peerShareCounts);

const hasPeers = computed(() => app.peers.length > 0);

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
  app.peerShareCounts = Object.fromEntries(results);
}

function peerCount(peer) {
  const count = shareCounts.value[peer.id];
  return typeof count === "number" ? count : null;
}

function savedPeer(server) {
  return findSavedPeer(app.peers, server);
}

function isSaved(server) {
  return Boolean(savedPeer(server));
}

/// 一台服务器的申请状态：读后端的内存快照，而不是本组件的临时变量，
/// 所以切换页面或重载后不会退回「可以申请」。
function rowState(server) {
  return accessRowState(findAccessRequest(app.accessRequests, server));
}

function requestMessage(server) {
  const state = rowState(server);
  if (state.message) return state.message;
  return state.messageKey ? t(state.messageKey) : "";
}

async function beginAdd(server) {
  const peer = savedPeer(server);
  if (peer) {
    app.openPeer(peer);
    return;
  }
  if (requestingUrl.value) {
    app.setNotice("home.requestInProgress");
    return;
  }

  requestingUrl.value = server.url;
  try {
    await app.beginPeerAccess(server.url, server.name);
  } catch (error) {
    app.handleFailure(error);
  } finally {
    requestingUrl.value = "";
  }
}

async function cancelRequest(server) {
  const entry = findAccessRequest(app.accessRequests, server);
  if (!entry || requestingUrl.value) return;
  requestingUrl.value = entry.url;
  try {
    await app.cancelPeerAccess(entry.url);
  } catch (error) {
    app.handleFailure(error);
  } finally {
    requestingUrl.value = "";
  }
}

/// 申请方的身份说明：机器码才是「哪台设备」，IP 只是它这次拿到的地址。
function pendingIdentityText(request) {
  const identity = pendingIdentity(request);
  return identity.trusted
    ? t("home.pendingIdentity", { code: identity.short })
    : t("home.pendingIdentityUnknown");
}

/// 审批后立即从列表消失：列表由共享上下文里的待审批数据驱动，审批完成即刷新。
async function approvePending(request) {
  if (approvingId.value) return;
  approvingId.value = request.id;
  try {
    await app.approvePendingRequest(request.id);
  } catch (error) {
    app.handleFailure(error);
  } finally {
    approvingId.value = "";
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
    <!-- 没有待审批申请时整块隐藏：空态提示只会占位置，没有可操作的内容。 -->
    <section v-if="pendingApprovals.length" class="home-section">
      <header class="section-heading">
        <h2>
          {{ t("home.pendingTitle") }}
          <span
            class="count-badge"
            :title="t('home.pendingTotal', { count: pendingApprovals.length })"
            >{{ pendingApprovals.length }}</span
          >
        </h2>
      </header>

      <ul class="server-list">
        <li
          v-for="request in pendingApprovals"
          :key="request.id"
          class="server-row"
        >
          <div class="server-row__line">
            <span class="server-row__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <path d="M12 3.6l7 3v5.1c0 4-2.9 7.5-7 8.7-4.1-1.2-7-4.7-7-8.7V6.6z" />
                <path d="M9.5 11.7l1.8 1.8 3.3-3.6" />
              </svg>
            </span>
            <div class="server-row__title">
              <strong>{{ request.peerIp }}</strong>
              <span class="server-row__url">{{ pendingIdentityText(request) }}</span>
            </div>
            <el-tag
              class="row-tag"
              :type="statusTagTone('pending')"
              effect="plain"
              round
              size="small"
            >
              {{ request.shareName }}
            </el-tag>
            <el-button
              class="list-pill"
              type="primary"
              round
              size="small"
              :disabled="approvingId === request.id"
              @click="approvePending(request)"
            >
              {{ t("home.pendingApprove") }}
            </el-button>
          </div>
        </li>
      </ul>
    </section>

    <section class="home-section">
      <header class="section-heading">
        <h2>
          {{ t("home.authorizedServers") }}
          <span
            v-if="hasPeers"
            class="count-badge"
            :title="t('home.totalServers', { count: app.peers.length })"
            >{{ app.peers.length }}</span
          >
        </h2>
      </header>

      <list-skeleton v-if="peersLoading" variant="card" :rows="2" />
      <ul v-else-if="hasPeers" class="server-list">
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
            <el-tag
              v-if="peerCount(peer) !== null"
              class="row-tag"
              type="info"
              effect="plain"
              round
              size="small"
            >
              {{ t("home.sharesCount", { count: peerCount(peer) }) }}
            </el-tag>
            <el-tag
              v-if="!peer.online"
              class="row-tag"
              :type="statusTagTone('offline')"
              effect="plain"
              round
              size="small"
            >
              {{ t("home.offline") }}
            </el-tag>
            <el-button
              class="list-pill"
              type="primary"
              round
              size="small"
              :disabled="removingPeerId === peer.id"
              @click.stop="forgetPeer(peer)"
            >
              {{ t("home.forget") }}
            </el-button>
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
        <h2>
          {{ t("home.discoveredServers") }}
          <span
            v-if="discovered.length"
            class="count-badge"
            :title="t('home.totalServers', { count: discovered.length })"
            >{{ discovered.length }}</span
          >
        </h2>
        <el-tag
          class="row-tag"
          :type="discoveryRunning ? statusTagTone('online') : statusTagTone('unknown')"
          effect="plain"
          round
          size="small"
        >
          {{
            discoveryRunning
              ? t("home.discoveryListening")
              : t("home.discoveryStopped")
          }}
        </el-tag>
      </header>

      <list-skeleton v-if="discoveredLoading" variant="card" :rows="3" />
      <ul v-else-if="discovered.length" class="server-list">
        <li
          v-for="server in discovered"
          :key="server.serverId"
          class="server-row"
        >
          <div class="server-row__line">
            <button
              type="button"
              class="server-row__main"
              :disabled="!isSaved(server) && !rowState(server).canRequest"
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
            <el-tag
              class="row-tag"
              type="info"
              effect="plain"
              round
              size="small"
            >
              {{ t("home.sharesCount", { count: server.shareCount }) }}
            </el-tag>
            <el-tag
              v-if="isSaved(server)"
              class="row-tag"
              :type="statusTagTone('connected')"
              effect="plain"
              round
              size="small"
            >
              {{ t("home.connected") }}
            </el-tag>
            <el-button
              v-else-if="rowState(server).canRequest"
              class="list-pill"
              type="primary"
              round
              size="small"
              :disabled="requestingUrl === server.url"
              @click="beginAdd(server)"
            >
              {{ t("home.add") }}
            </el-button>
            <el-button
              v-else-if="rowState(server).canCancel"
              class="list-pill"
              type="primary"
              round
              size="small"
              :disabled="Boolean(requestingUrl)"
              @click="cancelRequest(server)"
            >
              {{ t("home.requestCancel") }}
            </el-button>
            <span class="server-row__chevron" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <path d="M9 6l6 6-6 6" />
              </svg>
            </span>
          </div>
          <p
            v-if="requestMessage(server)"
            class="server-row__status"
            :class="{ 'is-error': rowState(server).status === 'error' }"
          >
            {{ requestMessage(server) }}
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
}
</style>
