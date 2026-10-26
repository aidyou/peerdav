<script setup>
import { computed, onMounted, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";

import { useAppContext } from "../lib/appContext";
import { confirmAction } from "../lib/confirm";
import { localizeMessage, t } from "../lib/i18n";
import { normalizeAppError } from "../lib/invoke";
import { validatePassword } from "../lib/password";
import { useAction } from "../composables/useAction";

const app = useAppContext();
const { busy, run } = useAction();

const settingsTabs = [
  { name: "shares", label: "settings.tabShares" },
  { name: "clients", label: "settings.tabClients" },
  { name: "service", label: "settings.tabService" },
  { name: "account", label: "settings.tabAccount" },
];

const autostartEnabled = ref(false);
const autostartLoading = ref(true);

const serverBind = ref("0.0.0.0");
const serverPort = ref(4918);
const serverDiscoveryPort = ref(4919);
const serverName = ref("PeerDav");

const shares = ref([]);
const authorizations = ref([]);

const shareName = ref("");
const shareRoot = ref("");
const shareAllow = ref([]);
const defaultAllowRules = ref([]);
const shareDeny = ref([]);
const shareCanRead = ref(true);
const shareCanWrite = ref(true);
const shareCanDelete = ref(false);
const editingShareId = ref("");
const showShareForm = ref(false);

const lanServers = ref([]);
const lanScanned = ref(false);
const manualOpen = ref(false);
const peerUrl = ref("");
const peerHealth = ref({});
const peerWaiting = ref(false);
const peerRequestId = ref("");
const peerRequestUrl = ref("");
const peerCancelRequested = ref(false);
const peerCanceling = ref(false);
const peerNoticeState = ref(null);

const adminCurrentPassword = ref("");
const adminNewPassword = ref("");
const adminNewPasswordConfirm = ref("");

const peerNotice = computed(() => {
  if (!peerNoticeState.value) return "";
  return t(peerNoticeState.value.key, peerNoticeState.value.params);
});

const serverStatusText = computed(() =>
  app.serverStatus.running
    ? t("server.running", { address: app.serverStatus.bindAddress ?? "" })
    : t("server.stopped"),
);

const serverDiscoveryText = computed(() =>
  app.serverStatus.discoveryRunning
    ? `:${app.serverStatus.discoveryPort}`
    : t("server.discoveryOff"),
);

function adminArgs() {
  return { username: app.adminUsername, password: app.serverPassword };
}

function setPeerNotice(key, params) {
  peerNoticeState.value = { key, params };
}

function parseRuleList(values, kind) {
  return values
    .map((value) => value.trim())
    .filter(Boolean)
    .map((value) => {
      let matcher;
      if (value.includes("/")) {
        matcher = { type: "network", value };
      } else if (/^[0-9a-f]{2}([:-][0-9a-f]{2}){5}$/i.test(value)) {
        matcher = { type: "mac", value };
      } else {
        matcher = { type: "ip", value };
      }
      return { kind, matcher };
    });
}

function shareFormRules() {
  return [
    ...parseRuleList(shareAllow.value, "allow"),
    ...parseRuleList(shareDeny.value, "deny"),
  ];
}

function shareFormPermissions() {
  return {
    canRead: shareCanRead.value,
    canWrite: shareCanWrite.value,
    canDelete: shareCanDelete.value,
  };
}

function resetShareForm() {
  editingShareId.value = "";
  shareName.value = "";
  shareRoot.value = "";
  shareAllow.value = [];
  shareDeny.value = [];
  shareCanRead.value = true;
  shareCanWrite.value = true;
  shareCanDelete.value = false;
}

function editShare(share) {
  editingShareId.value = share.id;
  shareName.value = share.name;
  shareRoot.value = share.rootPath;
  shareAllow.value = share.rules
    .filter((rule) => rule.kind === "allow")
    .map((rule) => rule.matcher.value);
  shareDeny.value = share.rules
    .filter((rule) => rule.kind === "deny")
    .map((rule) => rule.matcher.value);
  shareCanRead.value = share.permissions.canRead;
  shareCanWrite.value = share.permissions.canWrite;
  shareCanDelete.value = share.permissions.canDelete;
  showShareForm.value = true;
}

function cancelShareEdit() {
  resetShareForm();
  showShareForm.value = false;
}

function openNewShareForm() {
  resetShareForm();
  showShareForm.value = true;
}

function effectiveShareName() {
  const typed = shareName.value.trim();
  return typed || shareNameFromPath(shareRoot.value);
}

function shareNameFromPath(path) {
  const parts = path.trim().split(/[\\/]/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : "share";
}

async function chooseShareRoot() {
  try {
    const selected = await openDialog({
      directory: true,
      multiple: false,
      title: t("shares.localDir"),
    });
    if (typeof selected === "string" && selected) {
      shareRoot.value = selected;
      shareName.value = shareNameFromPath(selected);
    }
  } catch (error) {
    app.setRawError(error);
  }
}

async function loadLocalNetworks() {
  try {
    const networks = await app.invoke("local_networks");
    defaultAllowRules.value = networks;
  } catch (error) {
    app.handleFailure(error);
  }
}

function describeRule(rule) {
  const kind = rule.kind === "deny" ? t("rule.deny") : t("rule.allow");
  let type = t("rule.ip");
  if (rule.matcher.type === "network") {
    type = t("rule.network");
  } else if (rule.matcher.type === "mac") {
    type = t("rule.mac");
  }
  return `${kind} · ${type} · ${rule.matcher.value}`;
}

const activeShareIds = ref([]);

async function refreshAdminData() {
  await run(loadAdminData);
}

async function loadAdminData() {
  const args = adminArgs();
  shares.value = await app.invoke("list_shares", args);
  app.pendingRequests = await app.invoke("list_pending_requests", args);
  authorizations.value = await app.invoke("list_authorizations", args);
  await app.refreshServerStatus();
}

async function startServer() {
  await run(async () => {
    app.serverStatus = await app.invoke("start_server", {
      ...adminArgs(),
      bindAddress: serverBind.value,
      port: Number(serverPort.value),
      discoveryPort: Number(serverDiscoveryPort.value),
      serverName: serverName.value || null,
    });
    app.setNotice("server.started", {
      address: app.serverStatus.bindAddress ?? "",
    });
  });
}

async function stopServer() {
  await run(async () => {
    app.serverStatus = await app.invoke("stop_server", adminArgs());
    app.setNotice("server.stopNotice");
  });
}

async function saveShare() {
  await run(async () => {
    if (editingShareId.value) {
      const shareId = editingShareId.value;
      await app.invoke("update_share_permissions", {
        ...adminArgs(),
        shareId,
        permissions: shareFormPermissions(),
      });
      await app.invoke("set_share_rules", {
        ...adminArgs(),
        shareId,
        rules: shareFormRules(),
      });
      app.setNotice("shares.updated");
    } else {
      await app.invoke("create_share", {
        ...adminArgs(),
        name: effectiveShareName(),
        rootPath: shareRoot.value,
        permissions: shareFormPermissions(),
        rules: shareFormRules(),
      });
      app.setNotice("shares.created");
    }
    resetShareForm();
    showShareForm.value = false;
    await loadAdminData();
  });
}

async function deleteShare(share) {
  await run(async () => {
    const confirmed = await confirmAction(
      t("shares.deleteConfirm", { name: share.name }),
      t("common.delete"),
    );
    if (!confirmed) return;

    await app.invoke("delete_share", {
      ...adminArgs(),
      shareId: share.id,
    });
    if (editingShareId.value === share.id) {
      resetShareForm();
    }
    app.setNotice("shares.deleted");
    await loadAdminData();
  });
}

async function approveRequest(request) {
  await run(async () => {
    const result = await app.invoke("approve_pending_request", {
      ...adminArgs(),
      requestId: request.id,
    });
    app.setNotice(
      result.autoDelivery ? "pending.approvedAuto" : "pending.approved",
    );
    await loadAdminData();
  });
}

async function revokeAuthorization(authorization) {
  await run(async () => {
    const confirmed = await confirmAction(
      t("auth.revokeConfirm", { label: authorization.label }),
      t("auth.revoke"),
    );
    if (!confirmed) return;

    await app.invoke("revoke_authorization", {
      ...adminArgs(),
      authorizationId: authorization.id,
    });
    app.setNotice("auth.revoked");
    await loadAdminData();
  });
}

async function scanLan() {
  await run(async () => {
    lanServers.value = await app.invoke("scan_lan");
    lanScanned.value = true;
  });
}

async function requestPeerAccess(url = peerUrl.value) {
  const target = String(url ?? "").trim();
  if (!target || peerWaiting.value) return;
  peerWaiting.value = true;
  peerRequestUrl.value = target;
  peerRequestId.value = "";
  peerCancelRequested.value = false;
  setPeerNotice("client.waiting");
  try {
    const request = await app.invoke("request_peer_access", { url: target });
    if (!request.requested) {
      setPeerNotice("client.alreadyAccessible");
      return;
    }
    if (!request.requestId) {
      setPeerNotice("client.requestMissing");
      return;
    }

    peerRequestId.value = request.requestId;
    const result = await app.invoke("wait_for_peer_access", {
      url: target,
      requestId: request.requestId,
      label: null,
    });
    await app.refreshPeers();
    setPeerNotice("client.connected", { label: result.label });
  } catch (error) {
    if (
      peerCancelRequested.value ||
      normalizeAppError(error).code === "PEER_ACCESS_CANCELED"
    ) {
      setPeerNotice("client.canceled");
    } else {
      app.handleFailure(error);
      setPeerNotice("client.failed");
    }
  } finally {
    peerWaiting.value = false;
    peerRequestId.value = "";
    peerRequestUrl.value = "";
    peerCancelRequested.value = false;
    peerCanceling.value = false;
  }
}

async function cancelPeerRequest() {
  if (!peerRequestId.value || peerCanceling.value) return;
  peerCanceling.value = true;
  peerCancelRequested.value = true;
  try {
    await app.invoke("cancel_peer_access", {
      url: peerRequestUrl.value,
      requestId: peerRequestId.value,
    });
  } catch (error) {
    peerCancelRequested.value = false;
    peerCanceling.value = false;
    app.handleFailure(error);
    setPeerNotice("client.cancelFailed");
  }
}

function healthText(health) {
  if (!health) return "";
  const label = !health.online
    ? t("client.offline")
    : health.authorized
      ? t("client.onlineAuthorized", { latency: health.latencyMs ?? "—" })
      : t("client.onlineUnauthorized");
  const reason = health.message ? localizeMessage(health.message) : "";
  return reason && reason !== label ? `${label} · ${reason}` : label;
}

function isPeerSaved(url) {
  return app.peers.some((peer) => peer.url === url);
}

async function checkPeer(peer) {
  await run(async () => {
    const health = await app.invoke("check_peer_health", { peerId: peer.id });
    peerHealth.value = { ...peerHealth.value, [peer.id]: health };
  });
}

async function forgetPeer(peer) {
  await run(async () => {
    const confirmed = await confirmAction(
      t("client.forgetConfirm", { label: peer.label }),
      t("client.forget"),
    );
    if (!confirmed) return;

    await app.invoke("forget_peer", { peerId: peer.id });
    if (app.activePeerId === peer.id) {
      app.closePeer();
    }
    await app.refreshPeers();
    setPeerNotice("client.forgotten");
  });
}

async function changeAdminPassword() {
  app.clearMessages();
  const passwordError = validatePassword(adminNewPassword.value);
  if (passwordError) {
    app.setError(passwordError);
    return;
  }
  if (adminNewPassword.value !== adminNewPasswordConfirm.value) {
    app.setError("login.mismatch");
    return;
  }

  await run(async () => {
    const newPassword = adminNewPassword.value;
    const result = await app.invoke("change_admin_password", {
      username: app.adminUsername,
      currentPassword: adminCurrentPassword.value,
      newPassword,
    });
    app.completeAdminPassword(result, newPassword);
    adminCurrentPassword.value = "";
    adminNewPassword.value = "";
    adminNewPasswordConfirm.value = "";
    app.setNotice("login.updated");
  });
}

onMounted(() => {
  void loadLocalNetworks();
  void loadAutostart();
  void loadInitialData();
});

async function loadAutostart() {
  try {
    autostartEnabled.value = await isAutostartEnabled();
  } catch (error) {
    app.handleFailure(error);
  } finally {
    autostartLoading.value = false;
  }
}

async function changeAutostart(value) {
  try {
    if (value) {
      await enableAutostart();
    } else {
      await disableAutostart();
    }
    autostartEnabled.value = await isAutostartEnabled();
    app.setNotice(
      value ? "settings.autostartEnabled" : "settings.autostartDisabled",
    );
  } catch (error) {
    app.handleFailure(error);
  }
}

/// Initial load after sign-in keeps the login notice visible; user-triggered
/// refreshes go through `run` and clear stale messages first.
async function loadInitialData() {
  try {
    await loadAdminData();
  } catch (error) {
    app.handleFailure(error);
  }
}
</script>

<template>
  <div class="settings-view">
    <nav class="settings-tabbar" role="tablist" :aria-label="t('settings.title')">
      <button
        v-for="tab in settingsTabs"
        :key="tab.name"
        type="button"
        role="tab"
        :id="`tab-${tab.name}`"
        class="settings-tab"
        :class="{ 'is-active': app.settingsTab === tab.name }"
        :aria-selected="app.settingsTab === tab.name"
        :aria-controls="`pane-${tab.name}`"
        @click="app.settingsTab = tab.name"
      >
        {{ t(tab.label) }}
      </button>
    </nav>

      <section
        v-if="app.settingsTab === 'service'"
        id="pane-service"
        class="settings-pane"
        role="tabpanel"
        aria-labelledby="tab-service"
      >
        <div class="server-panel">
          <h3>{{ t("server.title") }}</h3>
          <p class="hint">{{ t("server.verifyHint") }}</p>
          <div class="grid">
            <label>
              {{ t("server.bindAddress") }}
              <el-input
                v-model="serverBind"
                type="text"
                placeholder="0.0.0.0"
              />
            </label>
            <label>
              {{ t("server.port") }}
              <el-input-number v-model="serverPort" :min="1" :max="65535" />
            </label>
            <label>
              {{ t("server.discoveryPort") }}
              <el-input-number
                v-model="serverDiscoveryPort"
                :min="1"
                :max="65535"
              />
            </label>
            <label>
              {{ t("server.name") }}
              <el-input
                v-model="serverName"
                type="text"
                placeholder="PeerDav"
              />
            </label>
          </div>
          <div class="actions">
            <el-button
              type="primary"
              :disabled="busy || app.serverStatus.running"
              @click="startServer"
            >
              {{ t("server.startService") }}
            </el-button>
            <el-button
              plain
              :disabled="busy || !app.serverStatus.running"
              @click="stopServer"
            >
              {{ t("server.stopService") }}
            </el-button>
            <el-button plain :disabled="busy" @click="refreshAdminData">
              {{ t("server.refresh") }}
            </el-button>
          </div>
          <p class="hint">
            {{ t("server.status") }}: {{ serverStatusText }} ·
            {{
              t("server.summary", {
                shares: app.serverStatus.shareCount,
                pending: app.serverStatus.pendingCount,
                discovery: serverDiscoveryText,
              })
            }}
          </p>
          <div class="settings-toggle">
            <div>
              <strong>{{ t("settings.autostart") }}</strong>
              <p class="hint">{{ t("settings.autostartHint") }}</p>
            </div>
            <el-switch
              :model-value="autostartEnabled"
              :loading="autostartLoading"
              :disabled="autostartLoading"
              @change="changeAutostart"
            />
          </div>
        </div>
      </section>

      <section
        v-if="app.settingsTab === 'shares'"
        id="pane-shares"
        class="settings-pane"
        role="tabpanel"
        aria-labelledby="tab-shares"
      >
        <div class="server-panel">
          <div class="pane-heading">
            <div class="pane-heading__title">
              <h3>{{ t("shares.title") }}</h3>
              <span class="pane-heading__badge">{{ shares.length }}</span>
            </div>
            <el-button
              type="primary"
              plain
              size="small"
              :aria-label="t('shares.create')"
              @click="openNewShareForm"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-heading__add-icon">
                <path d="M12 5v14M5 12h14" />
              </svg>
            </el-button>
          </div>
          <el-collapse v-if="shares.length" v-model="activeShareIds" class="share-collapse">
            <el-collapse-item
              v-for="share in shares"
              :key="share.id"
              :name="share.id"
            >
              <template #title>
                <div class="share-item__main">
                  <span class="share-item__icon" aria-hidden="true">
                    <svg viewBox="0 0 24 24">
                      <path
                        d="M3.5 7.5A2 2 0 0 1 5.5 5.5h3.6l2 2h7.4a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"
                      />
                    </svg>
                  </span>
                  <div class="share-item__title">
                    <strong>{{ share.name }}</strong>
                    <span class="hint mono">{{ share.rootPath }}</span>
                  </div>
                </div>
                <div class="share-item__perms">
                  <el-checkbox :model-value="share.permissions.canRead" disabled>
                    {{ t("permissions.read") }}
                  </el-checkbox>
                  <el-checkbox :model-value="share.permissions.canWrite" disabled>
                    {{ t("permissions.write") }}
                  </el-checkbox>
                  <el-checkbox :model-value="share.permissions.canDelete" disabled>
                    {{ t("permissions.delete") }}
                  </el-checkbox>
                </div>
                <div class="share-item__actions" @click.stop>
                  <el-button
                    type="primary"
                    plain
                    size="small"
                    round
                    :disabled="busy"
                    @click="editShare(share)"
                  >
                    {{ t("shares.edit") }}
                  </el-button>
                  <el-button
                    type="primary"
                    plain
                    size="small"
                    round
                    :disabled="busy"
                    @click="deleteShare(share)"
                  >
                    {{ t("common.delete") }}
                  </el-button>
                </div>
              </template>
              <ul v-if="share.rules.length" class="share-item__rules">
                <li
                  v-for="(rule, index) in share.rules"
                  :key="`${share.id}-${index}`"
                  class="hint"
                >
                  {{ describeRule(rule) }}
                </li>
              </ul>
            </el-collapse-item>
          </el-collapse>
          <p v-else class="hint">{{ t("shares.empty") }}</p>

          <p v-if="editingShareId" class="hint">
            {{ t("shares.editing", { name: shareName }) }}
          </p>
          <form v-if="showShareForm" class="stack" @submit.prevent="saveShare">
            <label>
              {{ t("shares.localDir") }}
              <div class="input-with-button">
                <el-input
                  v-model="shareRoot"
                  type="text"
                  :disabled="Boolean(editingShareId)"
                  required
                />
                <el-button
                  plain
                  :disabled="busy || Boolean(editingShareId)"
                  @click="chooseShareRoot"
                >
                  {{ t("shares.chooseDir") }}
                </el-button>
              </div>
            </label>
            <label>
              {{ t("shares.name") }}
              <el-input
                v-model="shareName"
                type="text"
                :disabled="Boolean(editingShareId)"
                :placeholder="t('shares.nameOptional')"
              />
            </label>
            <label>
              {{ t("shares.allowRules") }}
              <el-select
                v-model="shareAllow"
                class="rule-select"
                multiple
                filterable
                allow-create
                default-first-option
                collapse-tags
                collapse-tags-tooltip
                :placeholder="t('shares.rulesPlaceholder')"
              >
                <el-option
                  v-for="net in defaultAllowRules"
                  :key="net"
                  :label="net"
                  :value="net"
                />
              </el-select>
            </label>
            <label>
              {{ t("shares.denyRules") }}
              <el-select
                v-model="shareDeny"
                class="rule-select"
                multiple
                filterable
                allow-create
                default-first-option
                collapse-tags
                collapse-tags-tooltip
                :placeholder="t('shares.denyPlaceholder')"
              >
                <el-option
                  v-for="net in defaultAllowRules"
                  :key="net"
                  :label="net"
                  :value="net"
                />
              </el-select>
            </label>
            <p class="hint">{{ t("shares.rulesHint") }}</p>
            <div class="checks">
              <el-checkbox v-model="shareCanRead">
                {{ t("permissions.read") }}
              </el-checkbox>
              <el-checkbox v-model="shareCanWrite">
                {{ t("permissions.write") }}
              </el-checkbox>
              <el-checkbox v-model="shareCanDelete">
                {{ t("permissions.delete") }}
              </el-checkbox>
            </div>
            <div class="actions">
              <el-button
                type="primary"
                native-type="submit"
                :disabled="busy || !shareRoot.trim()"
              >
                {{ editingShareId ? t("shares.save") : t("shares.create") }}
              </el-button>
              <el-button
                v-if="editingShareId"
                plain
                :disabled="busy"
                @click="cancelShareEdit"
              >
                {{ t("shares.cancelEdit") }}
              </el-button>
            </div>
          </form>
        </div>
      </section>

      <section
        v-if="app.settingsTab === 'clients'"
        id="pane-clients"
        class="settings-pane"
        role="tabpanel"
        aria-labelledby="tab-clients"
      >
        <div class="client-roles">
          <section class="client-role">
            <header class="role-heading">
              <span class="role-icon" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <path d="M12 3v11" />
                  <path d="m8 10 4 4 4-4" />
                  <path d="M5 18h14" />
                </svg>
              </span>
              <div>
                <h2>{{ t("clients.inboundTitle") }}</h2>
                <p>{{ t("clients.inboundHint") }}</p>
              </div>
            </header>

            <div class="role-card">
              <div class="role-block">
                <h3>{{ t("pending.title") }}</h3>
                <p class="hint">{{ t("pending.hint") }}</p>
                <ul class="list">
                  <li v-for="request in app.pendingRequests" :key="request.id">
                    <div>
                      <strong class="mono">{{ request.peerIp }}</strong>
                      <div class="hint">
                        {{ request.shareName }}
                        <template v-if="request.attempts > 1">
                          · {{ t("pending.attempts", { count: request.attempts }) }}
                        </template>
                      </div>
                    </div>
                    <el-button
                      type="primary"
                      :disabled="busy"
                      @click="approveRequest(request)"
                    >
                      {{ t("pending.approve") }}
                    </el-button>
                  </li>
                  <li v-if="!app.pendingRequests.length" class="hint">
                    {{ t("pending.empty") }}
                  </li>
                </ul>
              </div>

              <div class="role-block">
                <h3>{{ t("auth.title") }}</h3>
                <p class="hint">{{ t("auth.hint") }}</p>
                <ul class="list">
                  <li
                    v-for="authorization in authorizations"
                    :key="authorization.id"
                  >
                    <div>
                      <strong>{{ authorization.label }}</strong>
                      <div class="hint">{{ t("auth.active") }}</div>
                    </div>
                    <el-button
                      plain
                      :disabled="busy"
                      @click.stop="revokeAuthorization(authorization)"
                    >
                      {{ t("auth.revoke") }}
                    </el-button>
                  </li>
                  <li v-if="!authorizations.length" class="hint">
                    {{ t("auth.empty") }}
                  </li>
                </ul>
              </div>
            </div>
          </section>

          <section class="client-role">
            <header class="role-heading">
              <span class="role-icon role-icon--outbound" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <path d="M12 17V6" />
                  <path d="m8 10 4-4 4 4" />
                  <path d="M5 20h14" />
                </svg>
              </span>
              <div>
                <h2>{{ t("clients.outboundTitle") }}</h2>
                <p>{{ t("clients.outboundHint") }}</p>
              </div>
            </header>

            <div class="role-card">
              <div class="role-block">
                <div class="role-block__head">
                  <div>
                    <h3>{{ t("client.addTitle") }}</h3>
                    <p class="hint">{{ t("client.addHint") }}</p>
                  </div>
                  <div class="actions">
                    <el-button
                      type="primary"
                      plain
                      :loading="busy"
                      :disabled="busy || peerWaiting"
                      @click="scanLan"
                    >
                      {{ t("client.scan") }}
                    </el-button>
                    <el-button plain @click="manualOpen = !manualOpen">
                      {{ t("client.manual") }}
                    </el-button>
                  </div>
                </div>

                <p v-if="lanScanned" class="hint">
                  {{
                    lanServers.length
                      ? t("client.scanFound", { count: lanServers.length })
                      : t("client.scanEmpty")
                  }}
                </p>

                <ul v-if="lanServers.length" class="list">
                  <li v-for="server in lanServers" :key="server.serverId">
                    <div>
                      <strong>{{ server.name }}</strong>
                      <div class="hint mono">{{ server.url }}</div>
                      <div class="hint">
                        {{
                          t("client.discoveredMeta", {
                            count: server.shareCount,
                            hosts: server.hosts.join(", "),
                          })
                        }}
                      </div>
                    </div>
                    <el-button
                      v-if="isPeerSaved(server.url)"
                      plain
                      disabled
                    >
                      {{ t("home.connected") }}
                    </el-button>
                    <el-button
                      v-else
                      type="primary"
                      plain
                      :disabled="busy || peerWaiting"
                      @click="requestPeerAccess(server.url)"
                    >
                      {{ t("client.connect") }}
                    </el-button>
                  </li>
                </ul>

                <form
                  v-if="manualOpen"
                  class="manual-connect"
                  @submit.prevent="requestPeerAccess()"
                >
                  <el-input
                    v-model="peerUrl"
                    type="text"
                    placeholder="http://192.168.1.10:4918"
                    :disabled="peerWaiting"
                  />
                  <el-button
                    type="primary"
                    native-type="submit"
                    :loading="peerWaiting"
                    :disabled="busy || peerWaiting || !peerUrl"
                  >
                    {{ t("client.request") }}
                  </el-button>
                </form>

                <div v-if="peerWaiting" class="request-progress">
                  <span>{{ peerNotice || t("client.waiting") }}</span>
                  <el-button
                    plain
                    :loading="peerCanceling"
                    @click="cancelPeerRequest"
                  >
                    {{ t("client.cancel") }}
                  </el-button>
                </div>
                <p v-else-if="peerNotice" class="hint request-progress__notice">
                  {{ peerNotice }}
                </p>
              </div>

              <div class="role-block">
                <h3>{{ t("client.savedTitle") }}</h3>
                <ul class="list">
                  <li v-for="peer in app.peers" :key="peer.id">
                    <div>
                      <strong>{{ peer.label }}</strong>
                      <div class="hint mono">{{ peer.url }}</div>
                      <div v-if="peerHealth[peer.id]" class="hint">
                        {{ healthText(peerHealth[peer.id]) }}
                      </div>
                    </div>
                    <div class="actions">
                      <el-button
                        type="primary"
                        :disabled="busy"
                        @click="app.openPeer(peer)"
                      >
                        {{ t("client.open") }}
                      </el-button>
                      <el-button plain :disabled="busy" @click="checkPeer(peer)">
                        {{ t("client.check") }}
                      </el-button>
                      <el-button
                        plain
                        :disabled="busy"
                        @click.stop="forgetPeer(peer)"
                      >
                        {{ t("client.forget") }}
                      </el-button>
                    </div>
                  </li>
                  <li v-if="!app.peers.length" class="hint">
                    {{ t("client.empty") }}
                  </li>
                </ul>
              </div>
            </div>
          </section>
        </div>
      </section>

      <section
        v-if="app.settingsTab === 'account'"
        id="pane-account"
        class="settings-pane"
        role="tabpanel"
        aria-labelledby="tab-account"
      >
        <div class="server-panel">
          <h3>{{ t("login.changeTitle") }}</h3>
          <p class="hint">{{ t("login.changeAccountHint") }}</p>
          <form class="stack" @submit.prevent="changeAdminPassword">
            <label>
              {{ t("login.username") }}
              <el-input :model-value="app.adminUsername" type="text" disabled />
            </label>
            <label>
              {{ t("login.currentPassword") }}
              <el-input
                v-model="adminCurrentPassword"
                type="password"
                show-password
                autocomplete="current-password"
                required
              />
            </label>
            <label>
              {{ t("login.newPassword") }}
              <el-input
                v-model="adminNewPassword"
                type="password"
                show-password
                minlength="6"
                autocomplete="new-password"
                required
              />
            </label>
            <label>
              {{ t("login.confirmNewPassword") }}
              <el-input
                v-model="adminNewPasswordConfirm"
                type="password"
                show-password
                minlength="6"
                autocomplete="new-password"
                required
              />
            </label>
            <div class="actions">
              <el-button type="primary" native-type="submit" :disabled="busy">
                {{ t("login.update") }}
              </el-button>
              <el-button plain :disabled="busy" @click="app.logout">
                {{ t("login.logout") }}
              </el-button>
            </div>
          </form>
        </div>

        <div class="server-panel">
          <h3>{{ t("vault.keyFileTitle") }}</h3>
          <p class="hint">{{ t("vault.keyFileHint") }}</p>
          <p v-if="app.status.keyFile" class="mono key-path">
            {{ app.status.keyFile }}
          </p>
          <div class="actions">
            <el-button plain :disabled="busy" @click="app.lockVault">
              {{ t("vault.lock") }}
            </el-button>
          </div>
        </div>
      </section>
  </div>
</template>

<style scoped>
.settings-view {
  max-width: 920px;
  margin: 0 auto;
}

.settings-tabbar {
  display: flex;
  width: fit-content;
  margin: 8px auto 18px;
  padding: 4px;
  border: 1px solid var(--app-border);
  border-radius: 13px;
  background: color-mix(in oklab, var(--app-surface-2) 74%, transparent);
}

.settings-tab {
  border: 0;
  border-radius: 9px;
  padding: 8px 50px;
  background: transparent;
  color: var(--app-text-secondary);
  font: inherit;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition:
    background 0.15s ease,
    color 0.15s ease,
    box-shadow 0.15s ease;
}

.settings-tab:hover {
  color: var(--app-text);
  background: var(--app-surface-3);
}

.settings-tab.is-active {
  color: #fff;
  background: var(--el-color-primary);
  box-shadow: 0 2px 8px color-mix(in oklab, var(--el-color-primary) 38%, transparent);
}

.settings-pane {
  display: flex;
  flex-direction: column;
}

.pane-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.pane-heading h3 {
  margin: 0;
}

.pane-heading__title {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.pane-heading__add-icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
}

.pane-heading__badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 22px;
  height: 20px;
  padding: 0 8px;
  border-radius: 10px;
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 12%, transparent);
  font-variant-numeric: tabular-nums;
}

.share-collapse {
  border-top: 0;
}

.share-collapse :deep(.el-collapse-item__header) {
  display: grid;
  grid-template-columns: 1fr auto 1fr auto;
  align-items: center;
  gap: 16px;
  height: auto;
  padding: 12px 0;
  line-height: 1.4;
  font-size: 14px;
  color: var(--app-text);
}

.share-collapse :deep(.el-collapse-item__arrow) {
  margin: 0;
}

.share-collapse :deep(.el-collapse-item__content) {
  padding-bottom: 4px;
}

.share-item__main {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.share-item__icon {
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--el-color-primary);
}

.share-item__icon svg {
  width: 32px;
  height: 32px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.share-item__title {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.share-item__title strong {
  font-size: 14px;
  color: var(--app-text);
}

.share-item__title .hint {
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.share-item__perms {
  display: flex;
  align-items: center;
  gap: 10px;
}

.share-item__perms :deep(.el-checkbox) {
  height: auto;
  margin: 0;
}

.share-item__perms :deep(.el-checkbox__label) {
  font-size: 13px;
  color: var(--app-text-secondary);
}

.share-item__actions {
  display: flex;
  align-items: center;
  gap: 8px;
  justify-self: end;
}

.share-item__rules {
  list-style: none;
  margin: 0;
  padding: 4px 0 8px 32px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.client-roles {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 18px;
  align-items: start;
}

.client-role {
  min-width: 0;
}

.role-heading {
  display: flex;
  align-items: flex-start;
  gap: 11px;
  margin-bottom: 10px;
  padding: 0 2px;
}

.role-heading h2 {
  margin: 0 0 4px;
  font-size: 17px;
  line-height: 1.3;
  color: var(--app-text);
}

.role-heading p {
  margin: 0;
  font-size: 12.5px;
  line-height: 1.55;
  color: var(--app-text-muted);
}

.role-icon {
  width: 32px;
  height: 32px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 10px;
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 10%, var(--app-surface-2));
}

.role-icon--outbound {
  color: color-mix(in oklab, var(--accent-green) 72%, var(--app-text));
  background: color-mix(in oklab, var(--accent-green) 10%, var(--app-surface-2));
}

.role-icon svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.role-card {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding: 16px;
  border: 1px solid var(--app-border);
  border-radius: 15px;
  background: var(--app-surface);
}

.role-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.role-block + .role-block {
  padding-top: 16px;
  border-top: 1px solid var(--app-border);
}

.role-block h3 {
  margin: 0;
  font-size: 14px;
  color: var(--app-text);
}

.role-block__head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 10px;
}

.role-block__head .actions {
  flex: none;
}

.manual-connect {
  display: flex;
  align-items: center;
  gap: 8px;
}

.manual-connect :deep(.el-input) {
  flex: 1;
  min-width: 0;
}

.request-progress {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 10px 11px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

.request-progress__notice {
  margin: 0;
}

.input-with-button {
  display: flex;
  align-items: center;
  gap: 8px;
}

.input-with-button :deep(.el-input) {
  flex: 1;
  min-width: 0;
}

.rule-select {
  width: 100%;
}

.settings-toggle {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
}

.settings-toggle strong {
  display: block;
  margin-bottom: 4px;
  font-size: 14px;
  color: var(--app-text-strong);
}

.key-path {
  overflow-wrap: anywhere;
}

@media (max-width: 760px) {
  .client-roles {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 560px) {
  .role-block__head {
    flex-direction: column;
  }

  .manual-connect {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>