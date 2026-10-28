<script setup>
import { computed, onMounted, ref, toRef } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  disable as disableAutostart,
  enable as enableAutostart,
  isEnabled as isAutostartEnabled,
} from "@tauri-apps/plugin-autostart";

import { stableList } from "../lib/listOrder";
import { useAppContext } from "../lib/appContext";
import { buildBindOptions } from "../lib/bindScope";
import { confirmAction } from "../lib/confirm";
import { toUserMessage } from "../lib/invoke";
import { locale, setLocale, localizeMessage, t } from "../lib/i18n";
import { theme, setTheme, SUPPORTED_THEMES } from "../lib/theme";
import { accent, setAccent, SUPPORTED_ACCENTS } from "../lib/accent";
import { generalSettings, saveGeneralSettings, shortcutFromEvent } from "../lib/general";
import { accessRowState, findAccessRequest, findSavedPeer } from "../lib/access";
import { statusTagTone } from "../lib/tagTone";
import { validatePassword } from "../lib/password";
import { useAction } from "../composables/useAction";
import { updateState, updateBusy, updateProgress, checkForUpdates } from "../lib/updater";
import trayMonoPreview from "../../design/icon/peerdav-tray-icon.svg";
import trayBluePreview from "../../design/icon/peerdav-tray-icon-blue.svg";
import trayDuoPreview from "../../design/icon/peerdav-tray-icon-duo.svg";
import ListSkeleton from "./ListSkeleton.vue";

const app = useAppContext();
const { busy, run } = useAction();

async function checkUpdates() {
  try { await checkForUpdates(); } catch (error) { app.handleFailure(error); }
}
async function installUpdate() {
  if (!await confirmAction(t("updates.confirm"), t("updates.install"), { confirmText: t("updates.install") })) return;
  try { await app.invoke("install_update"); } catch (error) { app.handleFailure(error); }
}

const capturingShortcut = ref(null);
function captureShortcut(event, field) {
  if (event.key === "Tab") return;
  event.preventDefault();
  event.stopPropagation();
  if (event.key === "Escape") { event.target.blur(); return; }
  const shortcut = shortcutFromEvent(event);
  if (!shortcut) return;
  const other = field === "toggleShortcut" ? "lockShortcut" : "toggleShortcut";
  if (generalSettings[other] === shortcut) { app.setError("lock.shortcutConflict"); return; }
  generalSettings[field] = shortcut;
  saveGeneralSettings();
  event.target.blur();
}
function clearShortcut(field) {
  generalSettings[field] = "";
  saveGeneralSettings();
}

const settingsTabs = [
  { name: "general", label: "settings.tabGeneral" },
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
const tcpEnabled = ref(false);
const tcpNetworks = ref([]);
const tcpOptions = computed(() => [...new Set([...defaultAllowRules.value, ...tcpNetworks.value])]);

const trayIconStyle = ref("duo");
const trayIconBusy = ref(true);
const trayIconOptions = [
  { value: "mono", label: "settings.trayIconMono", preview: trayMonoPreview },
  { value: "blue", label: "settings.trayIconBlue", preview: trayBluePreview },
  { value: "duo", label: "settings.trayIconDuo", preview: trayDuoPreview },
];

/* 语言名固定用各自语言书写（简体中文 / English），不随当前界面语言翻译，
   这是语言选择器的通用约定：切错语言后仍能认出自己的母语选项。 */
const languageOptions = [
  { value: "zh-CN", label: "language.zh" },
  { value: "en", label: "language.en" },
];

async function loadServiceSettings() {
  try {
    const settings = await app.invoke("service_settings");
    if (!settings) return;
    serverName.value = settings.name;
    serverBind.value = settings.bindAddress;
    serverPort.value = settings.port;
    serverDiscoveryPort.value = settings.discoveryPort;
    tcpEnabled.value = settings.tcp.enabled;
    tcpNetworks.value = settings.tcp.networks;
  } catch (error) { app.handleFailure(error); }
}

async function saveTcpSettings() {
  await run(async () => {
    await app.invoke("set_tcp_discovery", {
      ...adminArgs(), settings: { enabled: tcpEnabled.value, networks: tcpNetworks.value },
    });
  });
}

/* 复制机器码是纯前端动作，没有后端因果链：可用性问题单独给提示，
   真正的剪贴板拒绝则把原始 message 放进用户可见文案、完整信息进 detail。 */
async function copyMachineId() {
  const value = app.deviceFingerprint;
  if (!value) return;
  if (!navigator.clipboard?.writeText) {
    app.setError("settings.clipboardUnavailable");
    return;
  }
  try {
    await navigator.clipboard.writeText(value);
    app.setNotice("settings.machineIdCopied");
  } catch (error) {
    const reason = error?.message || String(error);
    app.handleFailure({
      code: "CLIPBOARD_WRITE_FAILED",
      message: t("settings.copyMachineIdFailed", { reason }),
      detail: error?.name ? `${error.name}: ${reason}` : reason,
    });
  }
}

async function loadTrayIconStyle() {
  try {
    trayIconStyle.value = await app.invoke("tray_icon_style");
  } catch (error) { app.handleFailure(error); }
  finally { trayIconBusy.value = false; }
}

async function changeTrayIconStyle(value) {
  if (trayIconBusy.value || value === trayIconStyle.value) return;
  trayIconBusy.value = true;
  try {
    await app.invoke("set_tray_icon_style", { style: value });
    trayIconStyle.value = value;
  } catch (error) {
    app.handleFailure(error);
  } finally { trayIconBusy.value = false; }
}

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

// 状态只影响展示与操作，不参与排序，避免审批或连接后设备行跳动。
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
    title: request.deviceName || request.peerIp,
    subtitle: "",
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
  const authorized = authorizations.value
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
  return stableList([...pending, ...authorized], (entry) => entry.request?.machineId ?? entry.authorization?.machineId ?? entry.key);
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
    identity: fingerprint ?? (peer ? peer.url : (server?.url ?? accessEntry?.url ?? "")),
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
  return stableList(rows, (entry) => entry.identity);
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
  await app.refreshAdminData();
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
    const discovered = findAccessRequest(app.accessRequests, { url: target });
    if (discovered?.fingerprint && !peerFingerprint.value.trim()) peerFingerprint.value = discovered.fingerprint;
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
  const reason = health.error ? toUserMessage(health.error) : health.message ? localizeMessage(health.message) : "";
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
    await Promise.all([app.refreshPeers(), app.refreshAdminData()]);
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
  void loadServiceSettings();
  void loadLocalNetworks();
  void loadLanAddress();
  void loadAutostart();
  void loadTrayIconStyle();
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

      <!-- 通用面板按语义分四组：外观、系统集成、锁屏、会话。每组是「组标题 + 行式列表」，
           行内标题在左、控件在右，一条设置项一行；服务、账户面板沿用同一套语法，
           成组的数据录入也走行式列表，提交动作收在列表尾行。
           面板级动作（立即锁屏、退出登录）挂在组标题右侧的小胶囊上，不占一行。 -->
      <section v-if="app.settingsTab === 'general'" id="pane-general" class="settings-pane" role="tabpanel" aria-labelledby="tab-general">
        <div class="settings-group">
          <h3 class="group-title">{{ t("settings.groupAppearance") }}</h3>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label" id="general-language-label">{{ t("language.label") }}</span>
              <div class="segmented-control" role="radiogroup" aria-labelledby="general-language-label">
                <button v-for="option in languageOptions" :key="option.value" type="button" role="radio"
                  class="segmented-control__option" :class="{ 'is-active': locale === option.value }"
                  :aria-checked="locale === option.value" @click="setLocale(option.value)">
                  {{ t(option.label) }}
                </button>
              </div>
            </div>
            <div class="settings-row">
              <span class="settings-row__label" id="general-theme-label">{{ t("theme.label") }}</span>
              <div class="segmented-control" role="radiogroup" aria-labelledby="general-theme-label">
                <button v-for="value in SUPPORTED_THEMES" :key="value" type="button" role="radio"
                  class="segmented-control__option" :class="{ 'is-active': theme === value }"
                  :aria-checked="theme === value" @click="setTheme(value)">
                  {{ t(`theme.${value}`) }}
                </button>
              </div>
            </div>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("accent.label") }}</span>
              <el-select class="row-control" :model-value="accent" @update:model-value="setAccent">
                <el-option v-for="value in SUPPORTED_ACCENTS" :key="value" :value="value" :label="t(`accent.${value}`)">
                  <!-- 色点只做识别辅助，选项名仍是文字，不靠颜色单独表达。 -->
                  <span class="accent-option"><i class="accent-dot" :style="{ background: `var(--accent-${value})` }" />{{ t(`accent.${value}`) }}</span>
                </el-option>
              </el-select>
            </label>
          </div>
          <p class="settings-group__footnote">{{ t("settings.appearanceHint") }}</p>
        </div>

        <div class="settings-group">
          <h3 class="group-title">{{ t("settings.groupIntegration") }}</h3>
          <div class="settings-rows">
            <div class="settings-row settings-row--stack tray-icon-settings">
              <div class="settings-row__text">
                <span class="settings-row__label" id="general-tray-icon-label">{{ t("settings.trayIcon") }}</span>
                <p class="hint">{{ t("settings.trayIconHint") }}</p>
              </div>
              <div class="segmented-control" role="radiogroup" aria-labelledby="general-tray-icon-label" :aria-busy="trayIconBusy">
                <button v-for="option in trayIconOptions" :key="option.value" type="button" role="radio"
                  class="segmented-control__option" :class="{ 'is-active': trayIconStyle === option.value }"
                  :aria-label="t(option.label)" :aria-checked="trayIconStyle === option.value"
                  :disabled="trayIconBusy" @click="changeTrayIconStyle(option.value)">
                  <img :src="option.preview" alt="" class="tray-icon-preview" :class="{ 'tray-icon-preview--mono': option.value === 'mono' }" />
                  {{ t(option.label) }}
                </button>
              </div>
            </div>
            <div class="settings-row settings-row--stack">
              <div class="settings-row__text">
                <span class="settings-row__label" id="general-autostart-label">{{ t("settings.autostart") }}</span>
                <p class="hint">{{ t("settings.autostartHint") }}</p>
              </div>
              <el-switch :model-value="autostartEnabled" :loading="autostartLoading" :disabled="autostartLoading"
                aria-labelledby="general-autostart-label" @change="changeAutostart" />
            </div>
          </div>
        </div>

        <div class="settings-group">
          <div class="settings-group__head">
            <h3 class="group-title">{{ t("settings.groupLock") }}</h3>
            <el-button class="list-pill group-action" round size="small" @click="app.lockInterface">{{ t("lock.now") }}</el-button>
          </div>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label" id="general-auto-lock-label">{{ t("lock.auto") }}</span>
              <el-switch v-model="generalSettings.autoLock" aria-labelledby="general-auto-lock-label" @change="saveGeneralSettings" />
            </div>
            <!-- 等待时长只在自动锁屏打开时生效：与其它行同列同字重，靠置灰表达依赖，不做缩进。 -->
            <label class="settings-row settings-row--sub" :class="{ 'is-dependent': !generalSettings.autoLock }">
              <span class="settings-row__label">{{ t("lock.minutes") }}</span>
              <span class="settings-row__control">
                <el-input-number v-model="generalSettings.idleMinutes" :min="5" :step="5" step-strictly :max="1440" :disabled="!generalSettings.autoLock" @change="saveGeneralSettings" />
                <span class="settings-row__unit">{{ t("settings.unitMinutes") }}</span>
              </span>
            </label>
            <div class="settings-row" v-for="field in ['toggleShortcut', 'lockShortcut']" :key="field">
              <span class="settings-row__label">{{ t(field === 'toggleShortcut' ? 'lock.toggleShortcut' : 'lock.shortcut') }}</span>
              <el-input class="row-control" :model-value="capturingShortcut === field ? '' : generalSettings[field]" readonly
                :aria-label="t(field === 'toggleShortcut' ? 'lock.toggleShortcut' : 'lock.shortcut')"
                :placeholder="t(capturingShortcut === field ? 'lock.recordingShortcut' : 'lock.captureShortcut')"
                @focus="capturingShortcut = field" @blur="capturingShortcut = null"
                @keydown="captureShortcut($event, field)">
                <template #append><el-button :aria-label="t('lock.clearShortcut')" @click="clearShortcut(field)">{{ t('lock.clearShortcut') }}</el-button></template>
              </el-input>
            </div>
          </div>
          <p class="settings-group__footnote">{{ t("lock.passwordHint") }}</p>
        </div>

        <div class="settings-group" data-testid="ignore-settings">
          <h3 class="group-title">{{ t("ignore.title") }}</h3>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label" id="respect-gitignore-label">{{ t("ignore.gitignore") }}</span>
              <el-switch v-model="generalSettings.respectGitignore" aria-labelledby="respect-gitignore-label" @change="saveGeneralSettings" />
            </div>
            <div class="settings-row settings-row--stack">
              <span class="settings-row__label" id="ignore-patterns-label">{{ t("ignore.patterns") }}</span>
              <el-input v-model="generalSettings.ignorePatterns" type="textarea" :autosize="{ minRows: 5, maxRows: 12 }"
                aria-labelledby="ignore-patterns-label" @change="saveGeneralSettings" />
            </div>
          </div>
          <p class="settings-group__footnote">{{ t("ignore.hint") }}</p>
        </div>

        <div class="settings-group">
          <h3 class="group-title">{{ t("updates.title") }}</h3>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label">{{ t("updates.current", { version: updateState.currentVersion }) }}</span>
              <el-button :loading="updateBusy" :disabled="updateBusy || !updateState.configured || updateState.phase === 'ready'" @click="checkUpdates">{{ t("updates.check") }}</el-button>
            </div>
            <div class="settings-row">
              <span class="settings-row__label" id="auto-update-label">{{ t("updates.auto") }}</span>
              <el-switch v-model="generalSettings.autoUpdate" aria-labelledby="auto-update-label" @change="saveGeneralSettings" />
            </div>
            <div class="settings-row" role="status" aria-live="polite">
              <span>{{ t(updateState.configured ? `updates.${updateState.phase}` : 'updates.unconfigured', { version: updateState.version, progress: updateProgress }) }}</span>
              <el-button v-if="updateState.phase === 'ready'" type="primary" @click="installUpdate">{{ t("updates.install") }}</el-button>
            </div>
          </div>
          <p class="settings-group__footnote">{{ t("updates.hint") }}</p>
          <p v-if="updateState.notes" class="settings-group__footnote" style="white-space: pre-wrap">{{ updateState.notes }}</p>
        </div>

        <div class="settings-group">
          <div class="settings-group__head">
            <h3 class="group-title">{{ t("settings.groupSession") }}</h3>
            <el-button class="list-pill group-action" round size="small" @click="app.logout">{{ t("login.logout") }}</el-button>
          </div>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label">{{ t("settings.currentAccount") }}</span>
              <span class="settings-row__value">{{ app.adminUsername }}</span>
            </div>
          </div>
        </div>
      </section>

      <!-- 服务面板同样按语义分组，并沿用通用面板的行式列表：
           「WebDAV 服务」放监听配置，「局域网发现」放主动探测，
           「本机标识」只读展示机器码。启动/停止与保存探测设置都收进各自
           列表的动作尾行并右对齐，让动作与它作用的字段看起来是同一张表单。 -->
      <section
        v-if="app.settingsTab === 'service'"
        id="pane-service"
        class="settings-pane"
        role="tabpanel"
        aria-labelledby="tab-service"
      >
        <div class="settings-group">
          <h3 class="group-title">{{ t("server.title") }}</h3>
          <div class="settings-rows">
            <label class="settings-row">
              <span class="settings-row__label">{{ t("server.name") }}</span>
              <el-input class="row-control" v-model="serverName" type="text" placeholder="PeerDav" />
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("server.bindAddress") }}</span>
              <el-select class="row-control bind-select" v-model="serverBind" filterable allow-create default-first-option>
                <el-option v-for="option in bindOptions" :key="option.value" :value="option.value" :label="option.text">
                  <span class="bind-option">
                    <span>{{ option.scope }}</span>
                    <span class="bind-option__addr">{{ option.address }}</span>
                  </span>
                </el-option>
              </el-select>
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("server.port") }}</span>
              <el-input-number class="row-control" v-model="serverPort" :min="1" :max="65535" />
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("server.discoveryPort") }}</span>
              <el-input-number class="row-control" v-model="serverDiscoveryPort" :min="1" :max="65535" />
            </label>
            <!-- 主命令收进同一张列表的尾行并右对齐，让它读起来是这个表单的提交行，
                 而不是浮在表单外的一排按钮。 -->
            <div class="settings-row settings-row--actions">
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
          </div>
          <p class="settings-group__footnote">{{ t("server.verifyHint") }}</p>
        </div>

        <div class="settings-group">
          <h3 class="group-title">{{ t("settings.groupDiscovery") }}</h3>
          <div class="settings-rows">
            <div class="settings-row settings-row--stack tcp-discovery-settings">
              <div class="settings-row__text">
                <span class="settings-row__label" id="service-tcp-label">{{ t("server.tcpDiscovery") }}</span>
                <p class="hint">{{ t("server.tcpHint") }}</p>
              </div>
              <el-switch v-model="tcpEnabled" aria-labelledby="service-tcp-label" />
            </div>
            <!-- 网段只在探测打开时生效：与其它行同列同字重，靠置灰表达依赖。 -->
            <label class="settings-row" :class="{ 'is-dependent': !tcpEnabled }">
              <span class="settings-row__label">{{ t("server.tcpNetworks") }}</span>
              <el-select class="row-control" v-model="tcpNetworks" multiple filterable allow-create
                :placeholder="t('server.tcpNetworksExample')" :disabled="!tcpEnabled">
                <el-option v-for="network in tcpOptions" :key="network" :label="network" :value="network" />
              </el-select>
            </label>
            <div class="settings-row settings-row--actions">
              <el-button plain :disabled="busy" @click="saveTcpSettings">{{ t("server.saveDiscovery") }}</el-button>
            </div>
          </div>
        </div>

        <div class="settings-group" v-if="app.deviceFingerprint">
          <h3 class="group-title">{{ t("settings.groupIdentity") }}</h3>
          <div class="settings-rows">
            <div class="settings-row">
              <span class="settings-row__label">{{ t("auth.machineId") }}</span>
              <!-- 机器码是 64 位十六进制，手抄不现实：整串做成可点击的文本按钮，
                   悬停提示与无障碍名都用同一句 i18n，点击即复制并回报结果。 -->
              <el-tooltip
                :content="t('settings.copyMachineId')"
                placement="top"
                :show-after="200"
                :hide-after="0"
                :enterable="false"
              >
                <button type="button" class="settings-row__value settings-row__copy mono"
                  :aria-label="t('settings.copyMachineId')" @click="copyMachineId">
                  {{ app.deviceFingerprint }}
                </button>
              </el-tooltip>
            </div>
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
        <div class="settings-group">
          <div class="settings-group__head">
            <h3 class="group-title">{{ t("shares.title") }}<span class="count-badge">{{ shares.length }}</span></h3>
            <el-button
              class="list-pill pane-cta"
              type="primary"
              round
              size="small"
              :aria-label="t('shares.create')"
              @click="openNewShareForm"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-cta__icon">
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
          <div v-else class="share-empty">
            <p class="hint">{{ t("shares.empty") }}</p>
          </div>

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
              <h3 class="group-title">
                {{ t("clients.inboundTitle") }}<span v-if="inboundEntries.length" class="count-badge">{{
                  inboundEntries.length
                }}</span>
              </h3>
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
            <!-- 分区说明收在列表下方做脚注：标题行只留识别，说明与设置面板同一套斜体小字。 -->
            <p class="settings-group__footnote">{{ t("clients.inboundHint") }}</p>
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
              <h3 class="group-title">
                {{ t("clients.outboundTitle") }}<span v-if="outboundEntries.length" class="count-badge">{{
                  outboundEntries.length
                }}</span>
              </h3>
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
            <div v-if="manualOpen && defaultAllowRules.length" class="hint manual-hint">
              {{ t("client.localNetworks") }}
              <el-tag v-for="network in defaultAllowRules" :key="network" type="info">
                {{ network }}
              </el-tag>
            </div>

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
                      <el-button class="list-pill" type="primary" round size="small" :disabled="busy" @click="run(() => app.removeLocalPeer(entry.peer))">
                        {{ t("client.removeLocal") }}
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
            <p class="settings-group__footnote">{{ t("clients.outboundHint") }}</p>
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
        <form class="settings-group" @submit.prevent="changeAdminPassword">
          <h3 class="group-title">{{ t("login.changeTitle") }}</h3>
          <div class="settings-rows">
            <label class="settings-row">
              <span class="settings-row__label">{{ t("login.username") }}</span>
              <el-input class="row-control" :model-value="app.adminUsername" type="text" disabled />
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("login.currentPassword") }}</span>
              <el-input class="row-control" v-model="adminCurrentPassword" type="password"
                show-password autocomplete="current-password" required />
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("login.newPassword") }}</span>
              <el-input class="row-control" v-model="adminNewPassword" type="password" show-password
                minlength="6" autocomplete="new-password" required />
            </label>
            <label class="settings-row">
              <span class="settings-row__label">{{ t("login.confirmNewPassword") }}</span>
              <el-input class="row-control" v-model="adminNewPasswordConfirm" type="password" show-password
                minlength="6" autocomplete="new-password" required />
            </label>
            <div class="settings-row settings-row--actions">
              <el-button type="primary" native-type="submit" :disabled="busy">
                {{ t("login.update") }}
              </el-button>
            </div>
          </div>
          <p class="settings-group__footnote">{{ t("login.changeAccountHint") }}</p>
        </form>

        <div class="settings-group">
          <h3 class="group-title">{{ t("vault.keyFileTitle") }}</h3>
          <div class="settings-rows">
            <!-- 路径可能很长，这一行的取值吃掉剩余宽度，选择按钮仍贴右边界。 -->
            <div class="settings-row settings-row--wide">
              <span class="settings-row__label">{{ t("vault.keyFilePath") }}</span>
              <div class="settings-row__control">
                <!-- 路径只读：换密钥只能通过选择文件完成，避免手输一个不存在的地址。 -->
                <el-input :model-value="app.status.keyFile ?? ''" type="text" readonly />
                <el-button plain :disabled="busy" @click="chooseVaultKey">
                  {{ t("vault.chooseKeyFile") }}
                </el-button>
              </div>
            </div>
          </div>
          <p class="settings-group__footnote">{{ t("vault.keyFileHint") }}</p>
        </div>
      </section>
  </div>
</template>

<style scoped>
.tray-icon-preview {
  width: 24px;
  height: 24px;
  object-fit: contain;
  vertical-align: middle;
  margin-inline-end: 6px;
}
html[data-theme="dark"] .tray-icon-preview--mono {
  filter: invert(1);
}
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

.pane-cta__icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
}

/* 通用、服务与账户面板共用的行式设置列表：组标题 + 一张描边列表 + 脚注/动作尾行。
   每条设置项一行，标题在左、控件在右，行尾就是当前状态或取值。 */
#pane-general,
#pane-service,
#pane-account {
  gap: 26px;
}

.settings-rows {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
  overflow: hidden;
}

/* 52px 行高容纳 segmented 控件（含 3px 内衬与描边）并留出点击余量。 */
.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  min-height: 52px;
  padding: 10px 14px;
}

.settings-row + .settings-row {
  border-top: 1px solid var(--app-border);
}

.settings-row__label {
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text-strong);
}

/* 带说明的行：标题仍承担识别，说明只补充"开启后会怎样"，放在标题下方。 */
.settings-row--stack {
  align-items: flex-start;
}

.settings-row__text {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* 依赖关系只靠置灰表达：不缩进、不降字重，跟其它行保持同一列起点。 */
.settings-row.is-dependent .settings-row__label,
.settings-row.is-dependent .settings-row__unit {
  color: var(--app-text-muted);
}

.settings-row__unit,
.settings-row__value {
  font-size: 13px;
  color: var(--app-text-secondary);
}

/* 动作尾行：不占标题列，控件右对齐，读起来是这张表单的提交行。 */
.settings-row--actions {
  justify-content: flex-end;
}

/* 机器码取值即按钮：去掉按钮外观，只保留文本的可点/可聚焦反馈，
   长串允许折行，避免在窄窗口把行撑破。 */
.settings-row__copy {
  max-width: 100%;
  padding: 0;
  border: 0;
  background: none;
  font: inherit;
  text-align: end;
  overflow-wrap: anywhere;
  cursor: pointer;
}

.settings-row__copy:hover {
  color: var(--el-color-primary);
  text-decoration: underline;
  text-underline-offset: 3px;
}

.settings-row__copy:focus-visible {
  outline: 2px solid var(--el-color-primary);
  outline-offset: 3px;
  border-radius: 4px;
}

/* 右侧控件列统一 240px：数字框加单位合起来的宽度，与下面快捷键输入框
   （含"清空"追加按钮）的总宽一致，两列右边界对齐。 */
.settings-row .row-control,
.settings-row .settings-row__control {
  width: 240px;
  max-width: 100%;
}

.settings-row__control {
  display: flex;
  align-items: center;
  gap: 8px;
}

.settings-row__control .el-input-number {
  flex: 1;
  min-width: 0;
}

.settings-row__control .el-input {
  flex: 1;
  min-width: 0;
}

/* 长取值（如密钥文件路径）吃掉剩余宽度，按钮仍贴右边界；其余行保持 240px 定列。 */
.settings-row--wide .settings-row__control {
  width: auto;
  flex: 1;
}

/* 注释性说明统一用斜体与小一号字，跟字段标题、取值区分开。 */
.settings-group__footnote {
  margin: 8px 2px 0;
  font-size: 12px;
  font-style: italic;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

/* 组尾动作上提到组标题右侧（小胶囊）：列表里不夹按钮，视线先落在标题行。 */
.settings-group__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

/* 头部自带 8px 下间距，标题在头部内不再叠加自己的 margin-bottom。 */
.settings-group__head .group-title {
  margin: 0;
}

/* 组标题右侧的动作胶囊默认就用主题色描边（与 hover 同色），让面板级动作在
   标题行就看得出来。只覆盖这两颗：行内 .list-pill 仍是中性描边，
   一屏几十颗强调色边框会把状态标签挤成噪音。 */
:root .el-button.list-pill.group-action {
  --el-button-border-color: color-mix(in oklab, var(--accent) 45%, var(--app-border));
}

.accent-option {
  display: flex;
  align-items: center;
  gap: 8px;
}

.accent-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  /* 浅色（黄）在白色下拉面板上会糊掉，描一圈内阴影保住轮廓；颜色只是辅助，选项名仍是文字。 */
  box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--app-text) 22%, transparent);
}

@media (max-width: 560px) {
  /* 窄窗口下左右两栏会把控件挤扁，改为标题在上、控件在下并撑满行宽。 */
  .settings-row {
    align-items: flex-start;
    flex-direction: column;
    gap: 8px;
  }

  .settings-row .row-control,
  .settings-row .settings-row__control,
  .settings-row .segmented-control {
    width: 100%;
  }

  .settings-row .segmented-control__option {
    flex: 1;
  }
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

/* 图标 + 组标题 + 可选动作，几何与设置面板的组标题一致（13px/700）。 */
.role-heading {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: center;
  gap: 11px;
  margin-bottom: 10px;
}

.role-heading .group-title {
  margin: 0;
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
.share-empty,
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

.share-empty .hint {
  margin: 0;
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

.tray-icon-settings .segmented-control {
  flex-shrink: 0;
}

.segmented-control {
  display: flex;
  padding: 3px;
  border: 1px solid var(--app-border);
  border-radius: 11px;
  background: color-mix(in oklab, var(--app-surface-2) 74%, transparent);
}

.segmented-control__option {
  border: 0;
  border-radius: 8px;
  padding: 6px 14px;
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

.segmented-control__option:hover {
  color: var(--app-text);
  background: var(--app-surface-3);
}

.segmented-control__option.is-active {
  color: #fff;
  background: var(--el-color-primary);
  box-shadow: 0 2px 8px color-mix(in oklab, var(--el-color-primary) 38%, transparent);
}

@media (max-width: 560px) {
  .manual-connect {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
