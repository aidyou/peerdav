<script setup>
import { computed, onMounted, ref, toRef } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";

import { useAppContext } from "../lib/appContext";
import { buildBindOptions } from "../lib/bindScope";
import { confirmAction } from "../lib/confirm";
import { localizeMessage, t } from "../lib/i18n";
import { accessRowState, findAccessRequest, findSavedPeer } from "../lib/access";
import { statusTagTone } from "../lib/tagTone";
import { validatePassword } from "../lib/password";
import { useAction } from "../composables/useAction";
import ListSkeleton from "./ListSkeleton.vue";

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

/* 绑定范围首选广域网/局域网/本机三项，其余本机网卡地址（VPN tun、多网卡）
   跟在后面；下拉框同时允许输入，公网地址或 IPv6 也能直接填。
   局域网地址由后端按默认路由排序给出，避免用户手输一个绑不上的地址。 */
const lanAddresses = ref([]);

const bindOptions = computed(() =>
  buildBindOptions({
    lanAddresses: lanAddresses.value,
    current: serverBind.value,
    labels: {
      wan: t("server.bindWan"),
      lan: t("server.bindLan"),
      local: t("server.bindLocal"),
      other: t("server.bindOther"),
      custom: t("server.bindCustom"),
    },
  }),
);

// 管理数据（共享、授权、待审批）由一次 loadAdminData 拉取；加载中的列表显示骨架屏。
// 数据本身存在共享上下文里：重新进入设置页先用旧数据渲染，后台刷新再替换，
// 不再每次进入都闪一次骨架屏。
const shares = toRef(app, "adminShares");
const authorizations = toRef(app, "adminAuthorizations");
const adminLoading = toRef(app, "adminLoading");

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

const lanServers = computed(() => app.discovered);
const activeInboundIds = ref([]);
const activeOutboundIds = ref([]);

/* 入站与出站共用同一套审批状态：待申请 / 待审批 / 已审批。
   排序把需要用户处理的放在前面，已完成配对放在最后。 */
const CLIENT_STATUS_RANK = {
  pending: 0,
  error: 0,
  canceled: 1,
  unrequested: 1,
  approved: 2,
};
const CLIENT_STATUS_LABEL = {
  pending: "clients.statusPending",
  error: "clients.statusFailed",
  canceled: "clients.statusCanceled",
  unrequested: "clients.statusUnrequested",
  approved: "clients.statusApproved",
};

function permissionText(permissions) {
  const parts = [];
  if (permissions.canRead) parts.push(t("permissions.read"));
  if (permissions.canWrite) parts.push(t("permissions.write"));
  if (permissions.canDelete) parts.push(t("permissions.delete"));
  return parts.join(" · ");
}

// 共享行里的读/写/删用自绘勾选框表达，不用 el-checkbox：复选框的 disabled 态
// 会把指针换成 🚫，鼠标扫过一行就像在提示「这里不许操作」，而这三项本来就只是状态。
function permissionFlags(share) {
  return [
    { key: "permissions.read", granted: share.permissions.canRead },
    { key: "permissions.write", granted: share.permissions.canWrite },
    { key: "permissions.delete", granted: share.permissions.canDelete },
  ];
}

const inboundEntries = computed(() => {
  const pending = app.pendingRequests.map((request) => ({
    key: `pending-${request.id}`,
    status: "pending",
    badgeKey: CLIENT_STATUS_LABEL.pending,
    action: "approve",
    title: request.peerIp,
    subtitle: request.shareName,
    request,
    details: [
      // 机器码才是设备身份；IP 只是它这次拿到的地址，所以两行都列出但语义不同。
      {
        label: t("pending.machineId"),
        value: request.machineId ?? t("pending.machineIdUnknown"),
        mono: true,
      },
      { label: t("pending.address"), value: request.peerIp, mono: true },
      { label: t("pending.requestedPath"), value: request.requestedPath, mono: true },
      { label: t("pending.firstSeen"), value: formatStamp(request.firstSeen) },
      { label: t("pending.lastSeen"), value: formatStamp(request.lastSeen) },
      { label: t("pending.attemptCount"), value: String(request.attempts) },
    ],
  }));
  const authorized = [...authorizations.value]
    .sort((a, b) => b.createdAt - a.createdAt)
    .map((authorization) => ({
      key: `auth-${authorization.id}`,
      status: "approved",
      badgeKey: CLIENT_STATUS_LABEL.approved,
      action: "revoke",
      title: authorization.label,
      subtitle: t("auth.authorizedAt", {
        time: formatStamp(authorization.createdAt),
      }),
      authorization,
      details: [
        {
          label: t("client.connectionState"),
          value: (app.inboundConnections ?? []).find((entry) => entry.fingerprint === authorization.machineId)?.activeConnections > 0
            ? t("client.online") : t("client.offline"),
        },
        { label: t("auth.idLabel"), value: authorization.id, mono: true },
        {
          label: t("auth.machineId"),
          value: authorization.machineId ?? t("pending.machineIdUnknown"),
          mono: true,
        },
      ],
    }));
  return [...pending, ...authorized];
});

function buildOutboundEntry(server, peer, accessEntry) {
  const row = accessRowState(accessEntry);
  // 「已连接」优先：申请只是通往连接的过程，已经连上时不再展示申请状态。
  const status = peer
    ? "approved"
    : ["pending", "error", "canceled"].includes(row.status)
      ? row.status
      : "unrequested";
  const details = [];
  const fingerprint = peer?.fingerprint ?? accessEntry?.fingerprint ?? server?.serverId;
  if (fingerprint) details.push({ label: t("auth.machineId"), value: fingerprint, mono: true });
  const snapshot = (app.outboundConnections ?? []).find((entry) => entry.fingerprint === fingerprint);
  if (peer) {
    details.push({
      label: t("client.connectionState"),
      value: (snapshot ? snapshot.state === "connected" : peer.online) ? t("client.online") : t("client.offline"),
    });
    details.push({
      label: t("client.shareCount"),
      value: String(peer.shares.length),
    });
  } else if (server) {
    details.push({
      label: t("client.shareCount"),
      value: String(server.shareCount),
    });
  }
  // 申请失败/过期的原因写进详情：切换页面回来仍然能看到，而不是只剩一次 toast。
  if (!peer && row.message) {
    details.push({ label: t("client.requestState"), value: row.message });
  }
  return {
    key: peer
      ? `peer-${peer.id}`
      : server
        ? `server-${server.serverId}`
        : `access-${accessEntry?.url ?? ""}`,
    status,
    badgeKey: CLIENT_STATUS_LABEL[status] ?? CLIENT_STATUS_LABEL.unrequested,
    action: peer
      ? "open"
      : row.canCancel
        ? "cancel"
        : row.canRequest
          ? "request"
          : "none",
    title: peer
      ? peer.label
      : (server?.name ?? accessEntry?.label ?? accessEntry?.url ?? ""),
    url: peer ? peer.url : (server?.url ?? accessEntry?.url ?? ""),
    hosts: server?.hosts ?? [],
    peer: peer ?? null,
    details,
  };
}

const outboundEntries = computed(() => {
  const rows = [];
  const coveredPeers = new Set();
  const coveredAccess = new Set();
  for (const server of lanServers.value) {
    const peer = findSavedPeer(app.peers, server);
    const accessEntry = findAccessRequest(app.accessRequests, server);
    if (peer) coveredPeers.add(peer.id);
    if (accessEntry) coveredAccess.add(accessEntry.url);
    rows.push(buildOutboundEntry(server, peer, accessEntry));
  }
  for (const peer of app.peers) {
    if (coveredPeers.has(peer.id)) continue;
    coveredPeers.add(peer.id);
    rows.push(buildOutboundEntry(null, peer, null));
  }
  // 手动输入、还没被广播发现的地址同样列出未完成的申请，否则它只存在于一次请求里。
  for (const entry of app.accessRequests) {
    if (coveredAccess.has(entry.url)) continue;
    if (app.peers.some((peer) => peer.url === entry.url)) continue;
    coveredAccess.add(entry.url);
    rows.push(buildOutboundEntry(null, null, entry));
  }
  return rows.sort(
    (a, b) => CLIENT_STATUS_RANK[a.status] - CLIENT_STATUS_RANK[b.status],
  );
});
// 骨架屏仅用于「还没有任何数据」的首屏；已有数据时的刷新继续渲染旧行，避免整块闪烁。
const sharesLoading = computed(
  () => adminLoading.value && shares.value.length === 0,
);
const inboundLoading = computed(
  () => adminLoading.value && inboundEntries.value.length === 0,
);
const outboundLoading = computed(
  () =>
    (app.peersLoading || app.discoveredLoading) &&
    outboundEntries.value.length === 0,
);

const manualOpen = ref(false);
const peerUrl = ref("");
const peerFingerprint = ref("");
const peerHealth = ref({});
// 只有「正在发这一条申请」的瞬时忙碌态留在组件里；申请结果由后端保存
// （app.accessRequests），所以切换页面/重载后不会退回「待申请」。
const requestingUrl = ref("");
const peerNoticeState = ref(null);

const manualEntry = computed(() =>
  peerUrl.value
    ? findAccessRequest(app.accessRequests, { url: peerUrl.value.trim() })
    : null,
);
const manualEntryMessage = computed(() => {
  const state = accessRowState(manualEntry.value);
  return state.message || (state.messageKey ? t(state.messageKey) : "");
});

const adminCurrentPassword = ref("");
const adminNewPassword = ref("");
const adminNewPasswordConfirm = ref("");

const peerNotice = computed(() => {
  if (!peerNoticeState.value) return "";
  return t(peerNoticeState.value.key, peerNoticeState.value.params);
});

function adminArgs() {
  return { username: app.adminUsername, password: app.serverPassword };
}

function setPeerNotice(key, params) {
  peerNoticeState.value = key ? { key, params } : null;
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

const shareDialogTitle = computed(() =>
  editingShareId.value ? t("shares.editTitle") : t("shares.createTitle"),
);

function cancelShareForm() {
  resetShareForm();
  showShareForm.value = false;
}

function openNewShareForm() {
  resetShareForm();
  showShareForm.value = true;
}

// The dialog close button bypasses the footer actions, so the form is cleared
// when the close transition ends. Opening another share before that must not
// wipe the freshly loaded values.
function onShareFormClosed() {
  if (showShareForm.value) return;
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
  } catch (error) {
    app.handleFailure(error);
  }
}

// 后端已把默认路由网卡排在最前，虚拟网桥不会顶掉真实局域网地址；
// 其余网卡（含 tun/VPN）全部保留，交给下拉框尾部和输入过滤。
async function loadLanAddress() {
  try {
    const addresses = await app.invoke("local_addresses");
    lanAddresses.value = Array.isArray(addresses) ? addresses : [];
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

async function loadAdminData() {
  adminLoading.value = true;
  try {
    const args = adminArgs();
    shares.value = await app.invoke("list_shares", args);
    app.pendingRequests = await app.invoke("list_pending_requests", args);
    authorizations.value = await app.invoke("list_authorizations", args);
    await app.refreshServerStatus();
  } finally {
    adminLoading.value = false;
  }
}

async function startServer() {
  await run(async () => {
    app.serverStatus = await app.invoke("start_server", {
      ...adminArgs(),
      bindAddress: serverBind.value.trim(),
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
    // 服务停了就没有待审批的人了，同步清掉控台上的申请行。
    await app.refreshPendingRequests();
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
  await run(() => app.approvePendingRequest(request.id));
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

/// 申请访问：申请结果记在后端（app.accessRequests），本组件只保留「正在发送」
/// 这一个瞬时标志，因此离开页面再回来状态不变。
async function requestPeerAccess(url = peerUrl.value) {
  const target = String(url ?? "").trim();
  if (!target || requestingUrl.value) return;
  requestingUrl.value = target;
  setPeerNotice(null);
  try {
    await app.beginPeerAccess(target, null, peerFingerprint.value.trim() || null);
  } catch (error) {
    app.handleFailure(error);
  } finally {
    requestingUrl.value = "";
  }
}

async function cancelPeerRequest(url = peerUrl.value) {
  const entry = findAccessRequest(app.accessRequests, {
    url: String(url ?? "").trim(),
  });
  if (!entry || requestingUrl.value) return;
  requestingUrl.value = entry.url;
  try {
    await app.cancelPeerAccess(entry.url);
  } catch (error) {
    app.handleFailure(error);
    setPeerNotice("client.cancelFailed");
  } finally {
    requestingUrl.value = "";
  }
}

function formatStamp(seconds) {
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

// 换密钥依赖后端先验再落：vault.reload_key 会先读取并校验候选密钥，只有
// 校验通过才写入密钥路径与内存密钥，失败则原样返回错误。因此选错文件时
// 当前密钥和路径都不变，用户不会被自己误选的密钥挡在登录之外。
async function chooseVaultKey() {
  const selected = await openDialog({
    multiple: false,
    directory: false,
    filters: [
      {
        name: t("vault.keyFileFilter"),
        extensions: ["key", "json"],
      },
    ],
  });
  if (typeof selected !== "string") return;

  await run(async () => {
    const next = await app.invoke("reload_vault_key", { path: selected });
    app.applyVaultStatus(next);
    app.setNotice("vault.keyLoaded");
  });
}

onMounted(() => {
  void loadLocalNetworks();
  void loadLanAddress();
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

/// Initial load after sign-in keeps the login notice visible; later reloads
/// happen right after a user action, inside that action's `run`.
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
        @click="app.selectSettingsTab(tab.name)"
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
          <p v-if="app.deviceFingerprint" class="hint">{{ t("auth.machineId") }}: {{ app.deviceFingerprint }}</p>
          <!-- 两列固定配对：名称+绑定地址一行，HTTP 服务端口与 UDP 广播端口一行。 -->
          <div class="grid pane-form-grid">
            <label>
              {{ t("server.name") }}
              <el-input
                v-model="serverName"
                type="text"
                placeholder="PeerDav"
              />
            </label>
            <label>
              {{ t("server.bindAddress") }}
              <el-select
                v-model="serverBind"
                class="bind-select"
                filterable
                allow-create
                default-first-option
              >
                <el-option
                  v-for="option in bindOptions"
                  :key="option.value"
                  :value="option.value"
                  :label="option.text"
                >
                  <span class="bind-option">
                    <span>{{ option.scope }}</span>
                    <span class="bind-option__addr">{{ option.address }}</span>
                  </span>
                </el-option>
              </el-select>
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
          </div>
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
              <span class="count-badge">{{ shares.length }}</span>
            </div>
            <el-button
              class="list-pill pane-cta"
              type="primary"
              round
              size="small"
              :aria-label="t('shares.create')"
              @click="openNewShareForm"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-heading__add-icon">
                <path d="M12 5v14M5 12h14" />
              </svg>
            </el-button>
          </div>
          <list-skeleton v-if="sharesLoading" :rows="3" />
          <el-collapse
            v-else-if="shares.length"
            v-model="activeShareIds"
            class="share-collapse"
          >
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
                  <span
                    v-for="flag in permissionFlags(share)"
                    :key="flag.key"
                    class="perm-flag"
                    :class="{ 'is-granted': flag.granted }"
                    role="img"
                    :aria-label="
                      t(
                        flag.granted
                          ? 'permissions.granted'
                          : 'permissions.denied',
                        { name: t(flag.key) },
                      )
                    "
                  >
                    <svg viewBox="0 0 16 16" aria-hidden="true" class="perm-flag__box">
                      <rect x="1.5" y="1.5" width="13" height="13" rx="3.5" />
                      <path v-if="flag.granted" d="M4.4 8.1l2.2 2.2 5-5.2" />
                    </svg>
                    {{ t(flag.key) }}
                  </span>
                </div>
                <div class="share-item__actions" @click.stop>
                  <el-button
                    class="list-pill"
                    type="primary"
                    size="small"
                    round
                    :disabled="busy"
                    :aria-label="t('shares.edit')"
                    @click="editShare(share)"
                  >
                    <svg
                      viewBox="0 0 24 24"
                      aria-hidden="true"
                      class="share-item__action-icon"
                    >
                      <path d="M4 20h4L19.5 8.5a2.12 2.12 0 0 0-3-3L5 17z" />
                      <path d="m14.5 6.5 3 3" />
                    </svg>
                  </el-button>
                  <el-button
                    class="list-pill"
                    type="primary"
                    size="small"
                    round
                    :disabled="busy"
                    :aria-label="t('common.delete')"
                    @click="deleteShare(share)"
                  >
                    <svg
                      viewBox="0 0 24 24"
                      aria-hidden="true"
                      class="share-item__action-icon"
                    >
                      <path d="M4 7h16" />
                      <path d="M9 7V4.5h6V7" />
                      <path d="m6.5 7 .8 13h9.4l.8-13" />
                      <path d="M10 11v5.5M14 11v5.5" />
                    </svg>
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

          <el-dialog
            v-model="showShareForm"
            :title="shareDialogTitle"
            width="520px"
            align-center
            :close-on-click-modal="false"
            :close-on-press-escape="false"
            @closed="onShareFormClosed"
          >
            <p v-if="editingShareId" class="hint">
              {{ t("shares.editing", { name: shareName }) }}
            </p>
            <form class="stack" @submit.prevent="saveShare">
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
              <p v-if="!shareAllow.length" class="hint share-form__warning">
                {{ t("shares.allowRequired") }}
              </p>
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
            </form>
            <template #footer>
              <el-button plain :disabled="busy" @click="cancelShareForm">
                {{ t("common.cancel") }}
              </el-button>
              <el-button
                type="primary"
                :disabled="busy || !shareRoot.trim()"
                @click="saveShare"
              >
                {{ editingShareId ? t("shares.save") : t("shares.create") }}
              </el-button>
            </template>
          </el-dialog>
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
              <div class="role-heading__text">
                <h2>
                  {{ t("clients.inboundTitle") }}
                  <span v-if="inboundEntries.length" class="count-badge">{{
                    inboundEntries.length
                  }}</span>
                </h2>
                <p>{{ t("clients.inboundHint") }}</p>
              </div>
            </header>

            <list-skeleton v-if="inboundLoading" :rows="3" />
            <el-collapse
              v-else-if="inboundEntries.length"
              v-model="activeInboundIds"
              class="client-collapse"
            >
              <el-collapse-item
                v-for="entry in inboundEntries"
                :key="entry.key"
                :name="entry.key"
              >
                <template #title>
                  <div class="client-item__main">
                    <span
                      class="client-item__icon"
                      :class="`is-${entry.status}`"
                      aria-hidden="true"
                    >
                      <svg v-if="entry.status === 'pending'" viewBox="0 0 24 24">
                        <circle cx="12" cy="12" r="8.5" />
                        <path d="M12 7.5V12l3.2 2" />
                      </svg>
                      <svg v-else viewBox="0 0 24 24">
                        <circle cx="8.5" cy="15.5" r="3.5" />
                        <path d="m11 13 8-8" />
                        <path d="m15.5 5 3 3" />
                      </svg>
                    </span>
                    <div class="client-item__title">
                      <strong>{{ entry.title }}</strong>
                      <span class="hint">{{ entry.subtitle }}</span>
                    </div>
                  </div>
                  <div class="client-item__status">
                    <el-tag
                      class="row-tag"
                      :type="statusTagTone(entry.status)"
                      effect="plain"
                      round
                      size="small"
                    >
                      {{ t(entry.badgeKey) }}
                    </el-tag>
                  </div>
                  <div class="client-item__actions" @click.stop>
                    <el-button
                      v-if="entry.action === 'approve'"
                      class="list-pill"
                      type="primary"
                      round
                      size="small"
                      :disabled="busy"
                      @click="approveRequest(entry.request)"
                    >
                      {{ t("pending.approve") }}
                    </el-button>
                    <el-button
                      v-else
                      class="list-pill"
                      type="primary"
                      round
                      size="small"
                      :disabled="busy"
                      @click="revokeAuthorization(entry.authorization)"
                    >
                      {{ t("auth.revoke") }}
                    </el-button>
                  </div>
                </template>
                <ul class="client-item__detail">
                  <li v-for="line in entry.details" :key="line.label">
                    <span>{{ line.label }}</span>
                    <strong :class="{ mono: line.mono }">{{ line.value }}</strong>
                  </li>
                </ul>
              </el-collapse-item>
            </el-collapse>
            <div v-else class="client-empty">
              <span class="client-empty__icon" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <circle cx="8.5" cy="15.5" r="3.5" />
                  <path d="m11 13 8-8" />
                  <path d="m15.5 5 3 3" />
                </svg>
              </span>
              <div>
                <strong>{{ t("clients.inboundEmptyTitle") }}</strong>
                <p class="hint">{{ t("clients.inboundEmptyHint") }}</p>
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
              <div class="role-heading__text">
                <h2>
                  {{ t("clients.outboundTitle") }}
                  <span v-if="outboundEntries.length" class="count-badge">{{
                    outboundEntries.length
                  }}</span>
                </h2>
                <p>{{ t("clients.outboundHint") }}</p>
              </div>
              <el-button
                class="list-pill"
                type="primary"
                round
                size="small"
                :aria-expanded="manualOpen"
                @click="manualOpen = !manualOpen"
              >
                {{ t("client.manual") }}
              </el-button>
            </header>

            <form
              v-if="manualOpen"
              class="manual-connect"
              @submit.prevent="requestPeerAccess()"
            >
              <el-input
                v-model="peerUrl"
                type="text"
                placeholder="https://192.168.1.10:4918"
                :disabled="Boolean(requestingUrl)"
              />
              <el-input
                v-model="peerFingerprint"
                :placeholder="t('client.fingerprint')"
                :aria-label="t('client.fingerprint')"
                :disabled="Boolean(requestingUrl)"
              />
              <el-button
                type="primary"
                native-type="submit"
                :loading="Boolean(requestingUrl)"
                :disabled="busy || Boolean(requestingUrl) || !peerUrl"
              >
                {{ t("client.request") }}
              </el-button>
            </form>
            <p v-if="manualOpen" class="hint manual-hint">
              {{ t("client.manualHint") }}
            </p>

            <!-- 只显示「正在发送」这种瞬时动作；申请结果来自后端状态，切换页面后仍在。 -->
            <div v-if="manualEntry?.status === 'waiting'" class="request-progress">
              <span>{{ t("client.waiting") }}</span>
              <el-button
                plain
                :disabled="Boolean(requestingUrl)"
                @click="cancelPeerRequest(manualEntry.url)"
              >
                {{ t("client.cancel") }}
              </el-button>
            </div>
            <p
              v-else-if="requestingUrl"
              class="hint request-progress__notice"
            >
              {{ t("client.sending") }}
            </p>
            <p
              v-else-if="manualEntryMessage || peerNotice"
              class="hint request-progress__notice"
            >
              {{ manualEntryMessage || peerNotice }}
            </p>

            <list-skeleton v-if="outboundLoading" :rows="3" />
            <el-collapse
              v-else-if="outboundEntries.length"
              v-model="activeOutboundIds"
              class="client-collapse"
            >
              <el-collapse-item
                v-for="entry in outboundEntries"
                :key="entry.key"
                :name="entry.key"
              >
                <template #title>
                  <div class="client-item__main">
                    <span
                      class="client-item__icon"
                      :class="`is-${entry.status}`"
                      aria-hidden="true"
                    >
                      <svg viewBox="0 0 24 24">
                        <rect x="4" y="4" width="16" height="6" rx="1.5" />
                        <rect x="4" y="14" width="16" height="6" rx="1.5" />
                        <path d="M8 7h.01M8 17h.01" />
                      </svg>
                    </span>
                    <div class="client-item__title">
                      <strong>{{ entry.title }}</strong>
                      <span class="hint mono">{{ entry.url }}</span>
                    </div>
                  </div>
                  <div class="client-item__status">
                    <el-tag
                      class="row-tag"
                      :type="statusTagTone(entry.status)"
                      effect="plain"
                      round
                      size="small"
                    >
                      {{ t(entry.badgeKey) }}
                    </el-tag>
                  </div>
                  <div class="client-item__actions" @click.stop>
                    <el-button
                      v-if="entry.action === 'request'"
                      class="list-pill"
                      type="primary"
                      round
                      size="small"
                      :disabled="busy || Boolean(requestingUrl)"
                      @click="requestPeerAccess(entry.url)"
                    >
                      {{ t("client.connect") }}
                    </el-button>
                    <el-button
                      v-else-if="entry.action === 'cancel'"
                      class="list-pill"
                      type="primary"
                      round
                      size="small"
                      :loading="requestingUrl === entry.url"
                      :disabled="Boolean(requestingUrl) && requestingUrl !== entry.url"
                      @click="cancelPeerRequest(entry.url)"
                    >
                      {{ t("client.cancel") }}
                    </el-button>
                    <template v-else-if="entry.peer">
                      <el-button
                        class="list-pill"
                        type="primary"
                        round
                        size="small"
                        :disabled="busy || !entry.peer.online"
                        @click="app.openPeer(entry.peer)"
                      >
                        {{ t("client.open") }}
                      </el-button>
                      <el-button
                        class="list-pill"
                        type="primary"
                        round
                        size="small"
                        :disabled="busy"
                        @click="forgetPeer(entry.peer)"
                      >
                        {{ t("client.forget") }}
                      </el-button>
                    </template>
                  </div>
                </template>
                <ul class="client-item__detail">
                  <li v-for="line in entry.details" :key="line.label">
                    <span>{{ line.label }}</span>
                    <strong :class="{ mono: line.mono }">{{ line.value }}</strong>
                  </li>
                </ul>
                <div v-if="entry.hosts.length" class="client-item__block">
                  <p class="hint client-item__block-title">
                    {{ t("client.hostsTitle") }}
                  </p>
                  <ul class="client-hosts">
                    <li v-for="host in entry.hosts" :key="host">{{ host }}</li>
                  </ul>
                </div>
                <div
                  v-if="entry.peer && entry.peer.shares.length"
                  class="client-item__block"
                >
                  <p class="hint client-item__block-title">
                    {{ t("client.grantedTitle") }}
                  </p>
                  <ul class="client-shares">
                    <li v-for="share in entry.peer.shares" :key="share.id">
                      <strong>{{ share.name }}</strong>
                      <span class="hint">
                        {{ permissionText(share.permissions) }}
                      </span>
                    </li>
                  </ul>
                </div>
                <div v-if="entry.peer" class="client-item__health">
                  <span class="hint">
                    {{
                      peerHealth[entry.peer.id]
                        ? healthText(peerHealth[entry.peer.id])
                        : t("client.healthUnknown")
                    }}
                  </span>
                  <el-button
                    class="list-pill"
                    type="primary"
                    round
                    size="small"
                    :disabled="busy"
                    @click="checkPeer(entry.peer)"
                  >
                    {{ t("client.check") }}
                  </el-button>
                </div>
              </el-collapse-item>
            </el-collapse>
            <div v-else class="client-empty">
              <span class="client-empty__icon" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <rect x="4" y="4" width="16" height="6" rx="1.5" />
                  <rect x="4" y="14" width="16" height="6" rx="1.5" />
                  <path d="M8 7h.01M8 17h.01" />
                </svg>
              </span>
              <div>
                <strong>{{ t("clients.outboundEmptyTitle") }}</strong>
                <p class="hint">{{ t("clients.outboundEmptyHint") }}</p>
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
            <!-- 两列配对：账号+当前密码一行，新密码+确认一行，避免单列拉成整屏长条。 -->
            <div class="grid pane-form-grid">
              <label>
                {{ t("login.username") }}
                <el-input
                  :model-value="app.adminUsername"
                  type="text"
                  disabled
                />
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
            </div>
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
          <!-- 用 .stack 包一层，label 才能拿到与其它面板一致的字段标题样式
               （13px/600 + 6px 间距），裸 label 会继承 16px 正文且贴住输入框。 -->
          <div class="stack">
            <label>
              {{ t("vault.keyFilePath") }}
              <!-- 路径只读：换密钥只能通过选择文件完成，避免手输一个不存在的地址。 -->
              <div class="input-with-button">
                <el-input
                  :model-value="app.status.keyFile ?? ''"
                  type="text"
                  readonly
                />
                <el-button plain :disabled="busy" @click="chooseVaultKey">
                  {{ t("vault.chooseKeyFile") }}
                </el-button>
              </div>
            </label>
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

/* 面板内表单固定两列，保证字段按语义成对换行；
   全局 .grid 用 auto-fit，宽窗口下会排出三列把配对打散。 */
.grid.pane-form-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

/* EP 的 select 默认 240px 固定宽，与同排输入框对不齐。 */
.bind-select {
  width: 100%;
}

.bind-option {
  display: flex;
  justify-content: space-between;
  gap: 16px;
}

.bind-option__addr {
  color: var(--app-text-secondary);
  font-variant-numeric: tabular-nums;
}

/* 列表/标题区按钮统一用全局 .list-pill（24px 胶囊）：App.vue 的 `:root .el-button`
   把控件钉在 38px，size="small" 单独不生效；对话框内的表单按钮不加该类，仍与输入框同高。 */

/* 最外框统一描边并裁 5px 圆角；行分隔线自己画，避免与外框叠成双线 */
.share-collapse {
  border: 1px solid var(--app-border);
  border-radius: 8px;
  overflow: hidden;
}

.share-collapse :deep(.el-collapse-item),
.share-collapse :deep(.el-collapse-item__header) {
  border-bottom: 0;
}

.share-collapse :deep(.el-collapse-item:not(:last-child)) {
  border-bottom: 1px solid var(--app-border);
}

/* header 只有两个子元素：Element Plus 的 title 包裹层 + 箭头 */
.share-collapse :deep(.el-collapse-item__header) {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  height: auto;
  padding: 12px 15px;
  line-height: 1.4;
  font-size: 14px;
  color: var(--app-text);
}

/* #title 插槽内容被包进 .el-collapse-item__title，三栏必须打在它身上。
   中栏/右栏用固定下限宽度，保证所有行的「读写删」和按钮垂直对齐。 */
.share-collapse :deep(.el-collapse-item__title) {
  display: grid;
  grid-template-columns:
    minmax(0, 1fr) minmax(132px, max-content) minmax(58px, max-content);
  align-items: center;
  gap: 16px;
  min-width: 0;
}

.share-collapse :deep(.el-collapse-item__arrow) {
  margin: 0;
  justify-self: end;
}

.share-collapse :deep(.el-collapse-item__content) {
  padding: 0 15px 4px;
}

.share-item__main {
  display: flex;
  align-items: center;
  gap: 5px;
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

.share-form__warning {
  color: var(--el-color-warning);
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
  gap: 12px;
}

.perm-flag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  font-weight: 500;
  color: var(--app-text-muted);
}

.perm-flag.is-granted {
  color: var(--tone-success);
}

.perm-flag__box {
  width: 14px;
  height: 14px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.share-item__actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.share-item__action-icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.share-item__rules {
  list-style: none;
  margin: 0;
  /* 与名称列起点对齐：图标 32px + 间距 5px */
  padding: 4px 0 8px 37px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.client-roles {
  display: grid;
  grid-template-columns: 1fr;
  gap: 40px;
  align-items: start;
}

.client-role {
  min-width: 0;
}

.role-heading {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: start;
  gap: 11px;
  margin-bottom: 12px;
  padding: 0 2px;
}

.role-heading__text {
  min-width: 0;
}

.role-heading h2 {
  margin: 0 0 4px;
  font-size: 16px;
  line-height: 1.3;
  color: var(--app-text);
}

.role-heading p {
  margin: 0;
  font-size: 12px;
  line-height: 1.6;
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

/* 客户端折叠面板沿用共享策略面板的语言：外框描边裁圆、行分隔自绘、
   折叠头三栏对齐。折叠头只放标识两行，状态与动作各占一栏，
   网段/规则/共享等长内容一律进展开体，避免一行里堆太多东西。 */
.client-collapse {
  border: 1px solid var(--app-border);
  border-radius: 8px;
  overflow: hidden;
}

.client-collapse :deep(.el-collapse-item),
.client-collapse :deep(.el-collapse-item__header) {
  border-bottom: 0;
}

.client-collapse :deep(.el-collapse-item:not(:last-child)) {
  border-bottom: 1px solid var(--app-border);
}

.client-collapse :deep(.el-collapse-item__header) {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: 12px;
  height: auto;
  padding: 10px 15px;
  line-height: 1.4;
  font-size: 14px;
  color: var(--app-text);
}

.client-collapse :deep(.el-collapse-item__title) {
  display: grid;
  grid-template-columns:
    minmax(0, 1fr) minmax(72px, max-content) minmax(58px, max-content);
  align-items: center;
  gap: 16px;
  min-width: 0;
}

.client-collapse :deep(.el-collapse-item__arrow) {
  margin: 0;
  justify-self: end;
}

.client-collapse :deep(.el-collapse-item__wrap) {
  background: var(--app-surface-2);
}

.client-collapse :deep(.el-collapse-item__content) {
  padding: 12px 15px 14px;
}

/* 空态与折叠面板同款外框，保证同一套设计语言 */
.client-empty {
  display: flex;
  align-items: center;
  gap: 12px;
  box-sizing: border-box;
  width: 100%;
  padding: 14px 15px;
  border: 1px solid var(--app-border);
  border-radius: 8px;
  background: var(--app-surface);
}

.client-empty__icon {
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  border: 1px solid var(--app-border);
  border-radius: 8px;
  color: var(--app-text-muted);
  background: var(--app-surface-2);
}

.client-empty__icon svg {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.client-empty strong {
  display: block;
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text);
}

.client-empty .hint {
  margin: 2px 0 0;
  font-size: 12px;
}

.client-item__main {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
}

.client-item__icon {
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--app-text-muted);
}

.client-item__icon.is-pending {
  color: var(--el-color-warning);
}

.client-item__icon.is-error {
  color: var(--tone-danger);
}

.client-item__icon.is-approved {
  color: var(--accent-green);
}

.client-item__icon svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.client-item__title {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.client-item__title strong {
  font-size: 14px;
  color: var(--app-text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.client-item__title .hint {
  font-size: 11.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.client-item__status {
  justify-self: start;
}

.client-item__actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.client-item__detail {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 6px;
}

.client-item__detail li {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  gap: 10px;
  font-size: 12.5px;
  line-height: 1.5;
}

.client-item__detail li > span {
  color: var(--app-text-muted);
}

.client-item__detail li > strong {
  color: var(--app-text-secondary);
  font-weight: 500;
  overflow-wrap: anywhere;
}

.client-item__block {
  margin-top: 10px;
}

.client-item__block-title {
  margin: 0 0 6px;
  font-size: 11.5px;
}

.client-hosts,
.client-shares {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.client-hosts li {
  padding: 2px 8px;
  border: 1px solid var(--app-border);
  border-radius: 6px;
  background: var(--app-surface);
  font-size: 11px;
  color: var(--app-text-secondary);
  overflow-wrap: anywhere;
}

.client-shares li {
  display: flex;
  align-items: baseline;
  gap: 6px;
  padding: 3px 9px;
  border: 1px solid var(--app-border);
  border-radius: 6px;
  background: var(--app-surface);
  font-size: 12px;
}

.client-shares strong {
  color: var(--app-text);
  font-weight: 500;
}

.client-item__health {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--app-border);
}

.client-item__health .hint {
  font-size: 12px;
}

.manual-connect {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
}

.manual-connect :deep(.el-input) {
  flex: 1;
  min-width: 0;
}

.manual-hint {
  margin: -8px 0 16px;
}

.request-progress {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 16px;
  padding: 10px 11px;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
  font-size: 12.5px;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

.request-progress__notice {
  margin: 0 0 16px;
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
  /* 与上面的表单拉开一档：面板 gap 14px + 这里 26px = 40px，
     开机启动是独立开关，不该看起来像表单里的一个字段。 */
  margin-top: 26px;
}

.settings-toggle strong {
  display: block;
  margin-bottom: 4px;
  font-size: 14px;
  color: var(--app-text-strong);
}

@media (max-width: 560px) {
  .manual-connect {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
