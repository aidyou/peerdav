<script setup>
import { computed, onMounted, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";

import { useAppContext } from "../lib/appContext";
import { localizeMessage, t } from "../lib/i18n";
import { validatePassword } from "../lib/password";
import { useAction } from "../composables/useAction";

const app = useAppContext();
const { busy, run } = useAction();

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
const approvalCode = ref("");
const approvalLabel = ref("");

const lanServers = ref([]);
const lanScanned = ref(false);
const peerUrl = ref("");
const peerCode = ref("");
const peerLabel = ref("");
const peerHealth = ref({});
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
  shareAllow.value = [...defaultAllowRules.value];
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
}

function cancelShareEdit() {
  resetShareForm();
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
    if (!editingShareId.value && !shareAllow.value.length) {
      shareAllow.value = [...networks];
    }
  } catch (error) {
    app.handleFailure(error);
  }
}

function describePermissions(permissions) {
  const labels = [];
  if (permissions.canRead) labels.push(t("permissions.read"));
  if (permissions.canWrite) labels.push(t("permissions.write"));
  if (permissions.canDelete) labels.push(t("permissions.delete"));
  return labels.length ? labels.join(" · ") : t("permissions.none");
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
    await loadAdminData();
  });
}

async function deleteShare(share) {
  if (!window.confirm(t("shares.deleteConfirm", { name: share.name }))) {
    return;
  }
  await run(async () => {
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
    approvalCode.value = result.authorizationCode;
    approvalLabel.value = `${request.peerIp} · ${request.shareName}`;
    app.setNotice("pending.approved");
    await loadAdminData();
  });
}

async function revokeAuthorization(authorization) {
  if (
    !window.confirm(t("auth.revokeConfirm", { label: authorization.label }))
  ) {
    return;
  }
  await run(async () => {
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

function useDiscovered(server) {
  peerUrl.value = server.url;
  setPeerNotice("client.filled", { url: server.url });
}

function peerUrlForHost(server, host) {
  const wrapped =
    host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  return `http://${wrapped}:${server.port}`;
}

function useDiscoveredHost(server, host) {
  peerUrl.value = peerUrlForHost(server, host);
  setPeerNotice("client.filled", { url: peerUrl.value });
}

async function requestPeerAccess() {
  await run(async () => {
    await app.invoke("request_peer_access", { url: peerUrl.value });
    setPeerNotice("client.requested");
  });
}

async function connectPeer() {
  await run(async () => {
    const result = await app.invoke("connect_peer", {
      url: peerUrl.value,
      code: peerCode.value,
      label: peerLabel.value || null,
    });
    peerCode.value = "";
    await app.refreshPeers();
    app.openPeer(result);
    setPeerNotice("client.connected", { label: result.label });
  });
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

async function checkPeer(peer) {
  await run(async () => {
    const health = await app.invoke("check_peer_health", { peerId: peer.id });
    peerHealth.value = { ...peerHealth.value, [peer.id]: health };
  });
}

async function forgetPeer(peer) {
  if (!window.confirm(t("client.forgetConfirm", { label: peer.label }))) {
    return;
  }
  await run(async () => {
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
    <header class="settings-header">
      <div>
        <h1>{{ t("settings.title") }}</h1>
        <p class="hint">
          {{ t("signedIn.current") }}:
          <strong class="mono">{{ app.adminUsername }}</strong>
        </p>
      </div>
      <label class="settings-secret">
        <span>{{ t("settings.adminPassword") }}</span>
        <el-input
          v-model="app.serverPassword"
          type="password"
          show-password
          autocomplete="current-password"
          :placeholder="t('server.needPassword')"
        />
      </label>
    </header>

    <el-tabs v-model="app.settingsTab" class="settings-tabs">
      <el-tab-pane :label="t('settings.tabService')" name="service">
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
      </el-tab-pane>

      <el-tab-pane :label="t('settings.tabShares')" name="shares">
        <div class="server-panel">
          <h3>{{ t("shares.title") }}</h3>
          <ul class="list">
            <li v-for="share in shares" :key="share.id">
              <div>
                <strong>{{ share.name }}</strong>
                <div class="hint mono">{{ share.rootPath }}</div>
                <div class="hint">
                  {{ describePermissions(share.permissions) }}
                </div>
                <div
                  v-for="(rule, index) in share.rules"
                  :key="`${share.id}-${index}`"
                  class="hint"
                >
                  {{ describeRule(rule) }}
                </div>
                <div v-if="!share.rules.length" class="hint">
                  {{ t("shares.noRules") }}
                </div>
              </div>
              <div class="actions">
                <el-button plain :disabled="busy" @click="editShare(share)">
                  {{ t("shares.edit") }}
                </el-button>
                <el-button plain :disabled="busy" @click="deleteShare(share)">
                  {{ t("common.delete") }}
                </el-button>
              </div>
            </li>
            <li v-if="!shares.length" class="hint">{{ t("shares.empty") }}</li>
          </ul>

          <p v-if="editingShareId" class="hint">
            {{ t("shares.editing", { name: shareName }) }}
          </p>
          <form class="stack" @submit.prevent="saveShare">
            <div class="grid">
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
            </div>
            <label>
              {{ t("shares.allowRules") }}
              <el-select
                v-model="shareAllow"
                class="rule-select"
                multiple
                filterable
                allow-create
                default-first-option
                :placeholder="t('shares.rulesPlaceholder')"
              />
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
                :placeholder="t('shares.denyPlaceholder')"
              />
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
              <el-button type="primary" native-type="submit" :disabled="busy">
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
      </el-tab-pane>

      <el-tab-pane :label="t('settings.tabClients')" name="clients">
        <div class="server-panel">
          <h3>{{ t("pending.title") }}</h3>
          <p class="hint">{{ t("pending.hint") }}</p>
          <ul class="list">
            <li v-for="request in app.pendingRequests" :key="request.id">
              <div>
                <strong class="mono">{{ request.peerIp }}</strong>
                <div class="hint">
                  {{ request.shareName }} · {{ request.requestedPath }} ·
                  {{ t("pending.attempts", { count: request.attempts }) }}
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

          <div v-if="approvalCode" class="code-box">
            <p class="warning">
              {{ t("code.onlyOnce") }}
              <span class="mono">{{ approvalLabel }}</span>
            </p>
            <p class="mono code">{{ approvalCode }}</p>
            <el-button plain @click="approvalCode = ''">
              {{ t("code.saved") }}
            </el-button>
          </div>
        </div>

        <div class="server-panel">
          <h3>{{ t("auth.title") }}</h3>
          <ul class="list">
            <li v-for="authorization in authorizations" :key="authorization.id">
              <div>
                <strong class="mono">{{ authorization.label }}</strong>
                <div class="hint mono">#{{ authorization.id }}</div>
              </div>
              <el-button
                plain
                :disabled="busy"
                @click="revokeAuthorization(authorization)"
              >
                {{ t("auth.revoke") }}
              </el-button>
            </li>
            <li v-if="!authorizations.length" class="hint">
              {{ t("auth.empty") }}
            </li>
          </ul>
        </div>

        <div class="server-panel">
          <h3>{{ t("client.title") }}</h3>
          <p class="hint">{{ t("client.hint") }}</p>
          <div class="actions">
            <el-button type="primary" :disabled="busy" @click="scanLan">
              {{ t("client.scan") }}
            </el-button>
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
                <div class="actions">
                  <el-button
                    v-for="host in server.hosts"
                    :key="host"
                    plain
                    :disabled="busy"
                    @click="useDiscoveredHost(server, host)"
                  >
                    {{ host }}
                  </el-button>
                </div>
              </div>
              <el-button plain :disabled="busy" @click="useDiscovered(server)">
                {{ t("client.use") }}
              </el-button>
            </li>
          </ul>

          <div class="grid">
            <label>
              {{ t("client.serverUrl") }}
              <el-input
                v-model="peerUrl"
                type="text"
                placeholder="http://192.168.1.10:4918"
              />
            </label>
            <label>
              {{ t("client.code") }}
              <el-input
                v-model="peerCode"
                type="password"
                show-password
                autocomplete="off"
              />
            </label>
            <label>
              {{ t("client.label") }}
              <el-input
                v-model="peerLabel"
                type="text"
                :placeholder="t('client.labelPlaceholder')"
              />
            </label>
          </div>
          <div class="actions">
            <el-button
              type="primary"
              :disabled="busy || !peerUrl"
              @click="requestPeerAccess"
            >
              {{ t("client.request") }}
            </el-button>
            <el-button
              :disabled="busy || !peerUrl || !peerCode"
              @click="connectPeer"
            >
              {{ t("client.connect") }}
            </el-button>
            <el-button plain :disabled="busy" @click="app.refreshPeers">
              {{ t("client.refreshSaved") }}
            </el-button>
          </div>
          <p v-if="peerNotice" class="hint">{{ peerNotice }}</p>

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
                <el-button plain :disabled="busy" @click="forgetPeer(peer)">
                  {{ t("client.forget") }}
                </el-button>
              </div>
            </li>
            <li v-if="!app.peers.length" class="hint">
              {{ t("client.empty") }}
            </li>
          </ul>
        </div>
      </el-tab-pane>

      <el-tab-pane :label="t('settings.tabAccount')" name="account">
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
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<style scoped>
.settings-view {
  max-width: 820px;
  margin: 0 auto;
}

.settings-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 20px;
  margin-bottom: 10px;
}

.settings-header h1 {
  margin: 0 0 6px;
  font-size: 22px;
  line-height: 1.2;
}

.settings-header .hint {
  margin: 0;
}

.settings-secret {
  flex: none;
  width: 240px;
  display: flex;
  flex-direction: column;
  gap: 5px;
  font-size: 12px;
  font-weight: 600;
  color: var(--app-text-secondary);
}

.settings-tabs {
  margin-top: 4px;
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

@media (max-width: 560px) {
  .settings-header {
    flex-direction: column;
    align-items: stretch;
  }

  .settings-secret {
    width: 100%;
  }
}
</style>