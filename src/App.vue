<script setup>
import { computed, onMounted, onUnmounted, ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { errorStrategy, invoke, normalizeAppError } from "./lib/invoke";
import { locale, localizeMessage, setLocale, t } from "./lib/i18n";

const status = ref({ initialized: false, unlocked: false });
const loading = ref(true);
const busy = ref(false);
const errorState = ref(null);
const noticeState = ref(null);
const showQuitDialog = ref(false);
const initAdminUsername = ref("");
const initAdminPassword = ref("");
const initAdminPasswordConfirm = ref("");
const initStep = ref(1);
const isMacOS = /Macintosh|Mac OS X/.test(navigator.userAgent);

const loggedIn = ref(false);
const adminUsername = ref("");
const mustChangeAdminPassword = ref(false);

const masterPassword = ref("");
const masterPasswordConfirm = ref("");
const unlockPassword = ref("");
const loginUsername = ref("");
const loginPassword = ref("");
const adminCurrentPassword = ref("");
const adminNewPassword = ref("");
const adminNewPasswordConfirm = ref("");
const currentMasterPassword = ref("");
const nextMasterPassword = ref("");
const nextMasterPasswordConfirm = ref("");
const showMasterPasswordForm = ref(false);

const serverStatus = ref({
  running: false,
  bindAddress: null,
  discoveryRunning: false,
  discoveryPort: null,
  shareCount: 0,
  pendingCount: 0,
});
const shares = ref([]);
const pendingRequests = ref([]);
const authorizations = ref([]);
const serverBind = ref("0.0.0.0");
const serverPort = ref(4918);
const serverDiscoveryPort = ref(4919);
const serverName = ref("PeerDav");
const serverPassword = ref("");
const shareName = ref("");
const shareRoot = ref("");
const shareAllow = ref("192.168.1.0/24");
const shareDeny = ref("");
const shareCanRead = ref(true);
const shareCanWrite = ref(true);
const shareCanDelete = ref(false);
const editingShareId = ref("");
const approvalCode = ref("");
const approvalLabel = ref("");

const lanServers = ref([]);
const lanScanned = ref(false);
const peers = ref([]);
const peerUrl = ref("");
const peerCode = ref("");
const peerLabel = ref("");
const peerHealth = ref({});
const activePeerId = ref("");
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

let eventUnlisten = [];
let statusRequestInFlight = false;

const errorMessage = computed(() => messageFromState(errorState.value));
const notice = computed(() => messageFromState(noticeState.value));
const peerNotice = computed(() => messageFromState(peerNoticeState.value));

function messageFromState(state) {
  if (!state) return "";
  if (state.key) return t(state.key, state.params);
  return localizeMessage(state.raw ?? "");
}

function setError(key, params) {
  errorState.value = { key, params };
  noticeState.value = null;
}

function setRawError(error) {
  errorState.value = { raw: normalizeAppError(error).message };
  noticeState.value = null;
}

function setNotice(key, params) {
  noticeState.value = { key, params };
}

function setPeerNotice(key, params) {
  peerNoticeState.value = { key, params };
}

function clearMessages() {
  errorState.value = null;
  noticeState.value = null;
}

const view = computed(() => {
  if (loading.value) return "loading";
  if (!status.value.initialized) return "initialize";
  if (!status.value.unlocked) return "unlock";
  if (mustChangeAdminPassword.value) return "change-admin";
  if (!loggedIn.value) return "login";
  return "ready";
});

const statusText = computed(() => {
  if (loading.value) return t("status.loading");
  if (!status.value.initialized) return t("status.notInitialized");
  if (!status.value.unlocked) return t("status.locked");
  return t("status.unlocked");
});

const statusClass = computed(() => ({
  "status-new": !loading.value && !status.value.initialized,
  "status-locked":
    !loading.value && status.value.initialized && !status.value.unlocked,
  "status-unlocked": !loading.value && status.value.unlocked,
}));

const serverStatusText = computed(() =>
  serverStatus.value.running
    ? t("server.running", { address: serverStatus.value.bindAddress ?? "" })
    : t("server.stopped"),
);

const serverDiscoveryText = computed(() =>
  serverStatus.value.discoveryRunning
    ? `:${serverStatus.value.discoveryPort}`
    : t("server.discoveryOff"),
);

const peerParentPath = computed(() => {
  if (!peerPath.value || peerPath.value === "/") return "/";
  const index = peerPath.value.lastIndexOf("/");
  return index <= 0 ? "/" : peerPath.value.slice(0, index);
});

const peerPreviewSource = computed(() => {
  if (!peerPreview.value || peerPreview.value.encoding !== "base64") return "";
  return `data:${peerPreview.value.mime};base64,${peerPreview.value.data}`;
});

onMounted(async () => {
  await bindServerEvents();
  await refreshStatus();
  await refreshServerStatus();
  try {
    await refreshPeers();
  } catch (error) {
    console.warn("PeerDav saved peers are unavailable", error);
  }
});

onUnmounted(() => {
  for (const unlisten of eventUnlisten) {
    unlisten();
  }
  eventUnlisten = [];
});

function runWindowAction(action) {
  return Promise.resolve()
    .then(action)
    .catch((error) => console.warn("PeerDav window action failed", error));
}

function closeWindow() {
  return runWindowAction(() => getCurrentWindow().close());
}

function minimizeWindow() {
  return runWindowAction(() => getCurrentWindow().minimize());
}

function toggleMaximizeWindow() {
  return runWindowAction(() => getCurrentWindow().toggleMaximize());
}

async function confirmQuit() {
  try {
    await invoke("quit_app");
  } catch (error) {
    console.warn("PeerDav quit failed", error);
  }
}

async function bindServerEvents() {
  try {
    eventUnlisten.push(
      await listen("app://pending-request", (event) => {
        const request = event.payload;
        if (!request?.id) return;
        if (!pendingRequests.value.some((item) => item.id === request.id)) {
          pendingRequests.value = [...pendingRequests.value, request];
        }
        serverStatus.value = {
          ...serverStatus.value,
          pendingCount: pendingRequests.value.length,
        };
        noticeState.value = {
          key: "pending.newRequest",
          params: { ip: request.peerIp },
        };
      }),
    );
    eventUnlisten.push(
      await listen("app://error", (event) => {
        errorState.value = { raw: normalizeAppError(event.payload).message };
      }),
    );
    eventUnlisten.push(
      await listen("app://quit-request", () => {
        showQuitDialog.value = true;
      }),
    );
  } catch (error) {
    console.warn("PeerDav event listeners are unavailable", error);
  }
}

async function refreshStatus() {
  if (statusRequestInFlight) return;
  statusRequestInFlight = true;
  try {
    status.value = await invoke("vault_status");
    if (!status.value.unlocked) {
      resetAdminSession();
    }
  } catch (error) {
    handleFailure(error);
  } finally {
    statusRequestInFlight = false;
    loading.value = false;
  }
}

async function run(action) {
  if (busy.value) return;
  busy.value = true;
  clearMessages();
  try {
    await action();
  } catch (error) {
    handleFailure(error);
  } finally {
    busy.value = false;
  }
}

function handleFailure(error) {
  setRawError(error);

  const strategy = errorStrategy(error);
  if (strategy === "unlock") {
    status.value = { ...status.value, unlocked: false };
    resetAdminSession();
  } else if (strategy === "initialize") {
    status.value = { initialized: false, unlocked: false };
    resetAdminSession();
  } else if (strategy === "refresh") {
    void refreshStatus();
  } else if (strategy === "change-admin") {
    mustChangeAdminPassword.value = true;
    loggedIn.value = false;
  } else if (strategy === "server-refresh") {
    void refreshServerStatus();
  } else if (strategy === "peer-refresh") {
    resetPeerView();
    void refreshPeers().catch((refreshError) => {
      const normalized = normalizeAppError(refreshError);
      console.error(`[peerdav:${normalized.code}] saved peers refresh failed`, {
        detail: normalized.detail,
        context: normalized.context,
      });
    });
  }
}

function resetAdminSession() {
  loggedIn.value = false;
  mustChangeAdminPassword.value = false;
  adminUsername.value = "";
  adminCurrentPassword.value = "";
  adminNewPassword.value = "";
  adminNewPasswordConfirm.value = "";
  serverPassword.value = "";
  shares.value = [];
  pendingRequests.value = [];
  authorizations.value = [];
  approvalCode.value = "";
  peerCode.value = "";
  peerPreview.value = null;
  peerEdit.value = null;
  editingShareId.value = "";
  resetPeerView();
}

const PASSWORD_MIN_LENGTH = 6;

function validatePassword(value) {
  if ([...value].length < PASSWORD_MIN_LENGTH) {
    return "vault.passwordTooShort";
  }
  if (value !== value.trim()) {
    return "vault.passwordWhitespace";
  }
  if (/[^\x20-\x7E]/.test(value)) {
    return "vault.passwordInvalidChars";
  }
  if (!/[A-Za-z]/.test(value)) {
    return "vault.passwordNeedLetter";
  }
  if (!/[0-9]/.test(value)) {
    return "vault.passwordNeedDigit";
  }
  return null;
}

function goToAdminStep() {
  errorState.value = null;
  const passwordError = validatePassword(masterPassword.value);
  if (passwordError) {
    setError(passwordError);
    return;
  }
  if (masterPassword.value !== masterPasswordConfirm.value) {
    setError("vault.masterMismatch");
    return;
  }
  initStep.value = 2;
}

function backToEncryptionStep() {
  clearMessages();
  initStep.value = 1;
}

async function initializeVault() {
  if (initStep.value === 1) {
    goToAdminStep();
    return;
  }

  errorState.value = null;
  const passwordError = validatePassword(initAdminPassword.value);
  if (passwordError) {
    setError(passwordError);
    return;
  }
  if (initAdminPassword.value !== initAdminPasswordConfirm.value) {
    setError("vault.adminPasswordMismatch");
    return;
  }

  await run(async () => {
    const result = await invoke("initialize_vault", {
      masterPassword: masterPassword.value,
      adminUsername: initAdminUsername.value,
      adminPassword: initAdminPassword.value,
    });
    masterPassword.value = "";
    masterPasswordConfirm.value = "";
    initAdminUsername.value = result.adminUsername;
    initAdminPassword.value = "";
    initAdminPasswordConfirm.value = "";
    initStep.value = 1;
    status.value = result.vaultStatus;
    loginUsername.value = result.adminUsername;
    resetAdminSession();
    setNotice("vault.initialized");
  });
}

async function unlockVault() {
  await run(async () => {
    status.value = await invoke("unlock_vault", {
      masterPassword: unlockPassword.value,
    });
    unlockPassword.value = "";
    setNotice("vault.unlocked");
  });
}

async function loginAdmin() {
  await run(async () => {
    const result = await invoke("admin_login", {
      username: loginUsername.value,
      password: loginPassword.value,
    });
    adminUsername.value = result.username;
    loginUsername.value = result.username;
    loginPassword.value = "";
    mustChangeAdminPassword.value = result.mustChangePassword;
    loggedIn.value = !result.mustChangePassword;
    setNotice(
      result.mustChangePassword ? "login.mustChange" : "login.succeeded",
    );
    await refreshServerStatus();
  });
}

async function changeAdminPassword() {
  errorState.value = null;
  const passwordError = validatePassword(adminNewPassword.value);
  if (passwordError) {
    setError(passwordError);
    return;
  }
  if (adminNewPassword.value !== adminNewPasswordConfirm.value) {
    setError("login.mismatch");
    return;
  }

  await run(async () => {
    const result = await invoke("change_admin_password", {
      username: adminUsername.value,
      currentPassword: adminCurrentPassword.value,
      newPassword: adminNewPassword.value,
    });
    mustChangeAdminPassword.value = result.mustChangePassword;
    loggedIn.value = true;
    serverPassword.value = "";
    adminCurrentPassword.value = "";
    adminNewPassword.value = "";
    adminNewPasswordConfirm.value = "";
    setNotice("login.updated");
    await refreshServerStatus();
  });
}

function backToLogin() {
  resetAdminSession();
  loginPassword.value = "";
  clearMessages();
}

async function changeMasterPassword() {
  errorState.value = null;
  const passwordError = validatePassword(nextMasterPassword.value);
  if (passwordError) {
    setError(passwordError);
    return;
  }
  if (nextMasterPassword.value !== nextMasterPasswordConfirm.value) {
    setError("vault.masterUpdateMismatch");
    return;
  }

  await run(async () => {
    status.value = await invoke("change_master_password", {
      currentPassword: currentMasterPassword.value,
      newPassword: nextMasterPassword.value,
    });
    currentMasterPassword.value = "";
    nextMasterPassword.value = "";
    nextMasterPasswordConfirm.value = "";
    showMasterPasswordForm.value = false;
    setNotice("vault.masterUpdated");
  });
}

async function lockVault() {
  await run(async () => {
    status.value = await invoke("lock_vault");
    unlockPassword.value = "";
    resetAdminSession();
    showMasterPasswordForm.value = false;
    currentMasterPassword.value = "";
    nextMasterPassword.value = "";
    nextMasterPasswordConfirm.value = "";
    setNotice("vault.locked");
  });
}

function adminArgs() {
  return {
    username: adminUsername.value,
    password: serverPassword.value,
  };
}

async function refreshServerStatus() {
  try {
    serverStatus.value = await invoke("server_status");
  } catch (error) {
    handleFailure(error);
  }
}

async function refreshAdminData() {
  if (!loggedIn.value || !serverPassword.value) {
    setError("server.needPassword");
    return;
  }
  await run(loadAdminData);
}

async function loadAdminData() {
  const args = adminArgs();
  shares.value = await invoke("list_shares", args);
  pendingRequests.value = await invoke("list_pending_requests", args);
  authorizations.value = await invoke("list_authorizations", args);
  await refreshServerStatus();
}

async function startServer() {
  await run(async () => {
    serverStatus.value = await invoke("start_server", {
      ...adminArgs(),
      bindAddress: serverBind.value,
      port: Number(serverPort.value),
      discoveryPort: Number(serverDiscoveryPort.value),
      serverName: serverName.value || null,
    });
    setNotice("server.started", {
      address: serverStatus.value.bindAddress ?? "",
    });
  });
}

async function stopServer() {
  await run(async () => {
    serverStatus.value = await invoke("stop_server", adminArgs());
    setNotice("server.stopNotice");
  });
}

async function scanLan() {
  await run(async () => {
    lanServers.value = await invoke("scan_lan");
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
    await invoke("request_peer_access", { url: peerUrl.value });
    setPeerNotice("client.requested");
  });
}

async function connectPeer() {
  await run(async () => {
    const result = await invoke("connect_peer", {
      url: peerUrl.value,
      code: peerCode.value,
      label: peerLabel.value || null,
    });
    peerCode.value = "";
    await refreshPeers();
    await selectPeer(result);
    setPeerNotice("client.connected", { label: result.label });
  });
}

async function refreshPeers() {
  peers.value = await invoke("list_peers");
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
    const health = await invoke("check_peer_health", { peerId: peer.id });
    peerHealth.value = { ...peerHealth.value, [peer.id]: health };
  });
}

async function openPeer(peer) {
  await run(async () => {
    await selectPeer(peer);
  });
}

async function selectPeer(peer) {
  activePeerId.value = peer.id;
  peerShares.value = await invoke("list_peer_shares", { peerId: peer.id });
  peerPreview.value = null;
  peerEdit.value = null;
  peerListing.value = null;
  peerPath.value = "/";
  activeShareId.value = "";
  if (peerShares.value.length) {
    await openPeerShare(peerShares.value[0].id);
  }
}

async function openPeerShare(shareId) {
  activeShareId.value = shareId;
  peerPath.value = "/";
  await browsePeer("/");
}

async function browsePeer(path) {
  if (!activePeerId.value || !activeShareId.value) return;
  await run(async () => {
    const listing = await invoke("list_peer_dir", {
      peerId: activePeerId.value,
      shareId: activeShareId.value,
      path,
    });
    peerListing.value = listing;
    peerPath.value = listing.path;
  });
}

async function forgetPeer(peer) {
  if (!window.confirm(t("client.forgetConfirm", { label: peer.label }))) {
    return;
  }
  await run(async () => {
    await invoke("forget_peer", { peerId: peer.id });
    if (activePeerId.value === peer.id) {
      resetPeerView();
    }
    await refreshPeers();
    setPeerNotice("client.forgotten");
  });
}

function enterPeerEntry(entry) {
  if (entry.isDir) {
    void browsePeer(entry.path);
  }
}

async function downloadEntry(entry) {
  if (!peerLocalDir.value.trim()) {
    setError("peer.downloadNeedDir");
    return;
  }
  await run(async () => {
    const result = await invoke("download_peer_file", {
      peerId: activePeerId.value,
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
    setError("peer.uploadNeedPath");
    return;
  }
  await run(async () => {
    const result = await invoke("upload_peer_file", {
      peerId: activePeerId.value,
      shareId: activeShareId.value,
      localPath: peerUploadPath.value,
      remoteDir: peerUploadDir.value || peerPath.value,
      conflict: peerConflict.value,
    });
    setPeerNotice("peer.uploaded", { path: result.remotePath });
    await browsePeer(peerPath.value);
  });
}

async function previewEntry(entry) {
  await run(async () => {
    peerPreview.value = await invoke("preview_peer_file", {
      peerId: activePeerId.value,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
  });
}

async function editEntry(entry) {
  await run(async () => {
    peerEdit.value = await invoke("prepare_peer_external_edit", {
      peerId: activePeerId.value,
      shareId: activeShareId.value,
      remotePath: entry.path,
    });
    setPeerNotice("peer.editOpened");
  });
}

async function commitPeerEdit() {
  if (!peerEdit.value) return;
  await run(async () => {
    const result = await invoke("commit_peer_external_edit", {
      editId: peerEdit.value.editId,
      conflict: peerConflict.value,
    });
    if (result.transferred) {
      setPeerNotice("peer.editUploaded", { path: result.remotePath });
    } else {
      setPeerNotice("peer.editNoChange");
    }
    peerEdit.value = null;
    await browsePeer(peerPath.value);
  });
}

async function discardPeerEdit() {
  if (!peerEdit.value) return;
  await run(async () => {
    await invoke("discard_peer_external_edit", {
      editId: peerEdit.value.editId,
    });
    peerEdit.value = null;
    setPeerNotice("peer.editDiscarded");
  });
}

function resetPeerView() {
  activePeerId.value = "";
  peerShares.value = [];
  activeShareId.value = "";
  peerPath.value = "/";
  peerListing.value = null;
  peerPreview.value = null;
  peerEdit.value = null;
}

function parseRuleList(text, kind) {
  return text
    .split(/[,，\n]/)
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
  shareAllow.value = "192.168.1.0/24";
  shareDeny.value = "";
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
    .map((rule) => rule.matcher.value)
    .join(", ");
  shareDeny.value = share.rules
    .filter((rule) => rule.kind === "deny")
    .map((rule) => rule.matcher.value)
    .join(", ");
  shareCanRead.value = share.permissions.canRead;
  shareCanWrite.value = share.permissions.canWrite;
  shareCanDelete.value = share.permissions.canDelete;
}

function cancelShareEdit() {
  resetShareForm();
}

async function saveShare() {
  await run(async () => {
    if (editingShareId.value) {
      const shareId = editingShareId.value;
      await invoke("update_share_permissions", {
        ...adminArgs(),
        shareId,
        permissions: shareFormPermissions(),
      });
      await invoke("set_share_rules", {
        ...adminArgs(),
        shareId,
        rules: shareFormRules(),
      });
      setNotice("shares.updated");
    } else {
      await invoke("create_share", {
        ...adminArgs(),
        name: shareName.value,
        rootPath: shareRoot.value,
        permissions: shareFormPermissions(),
        rules: shareFormRules(),
      });
      setNotice("shares.created");
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
    await invoke("delete_share", {
      ...adminArgs(),
      shareId: share.id,
    });
    if (editingShareId.value === share.id) {
      resetShareForm();
    }
    setNotice("shares.deleted");
    await loadAdminData();
  });
}

async function approveRequest(request) {
  await run(async () => {
    const result = await invoke("approve_pending_request", {
      ...adminArgs(),
      requestId: request.id,
    });
    approvalCode.value = result.authorizationCode;
    approvalLabel.value = `${request.peerIp} · ${request.shareName}`;
    setNotice("pending.approved");
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
    await invoke("revoke_authorization", {
      ...adminArgs(),
      authorizationId: authorization.id,
    });
    setNotice("auth.revoked");
    await loadAdminData();
  });
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
</script>

<template>
  <div class="window-frame">
    <header class="titlebar" data-tauri-drag-region>
      <div
        class="traffic-lights"
        :class="{ 'traffic-lights--windows': !isMacOS }"
      >
        <button
          type="button"
          class="traffic-light close"
          :aria-label="t('window.close')"
          :title="t('window.close')"
          @click="closeWindow"
        >
          <svg viewBox="0 0 12 12" aria-hidden="true">
            <path d="M3.2 3.2l5.6 5.6M8.8 3.2L3.2 8.8" />
          </svg>
        </button>
        <button
          type="button"
          class="traffic-light minimize"
          :aria-label="t('window.minimize')"
          :title="t('window.minimize')"
          @click="minimizeWindow"
        >
          <svg viewBox="0 0 12 12" aria-hidden="true">
            <path d="M3 6h6" />
          </svg>
        </button>
        <button
          type="button"
          class="traffic-light maximize"
          :aria-label="t('window.maximize')"
          :title="t('window.maximize')"
          @click="toggleMaximizeWindow"
        >
          <svg viewBox="0 0 12 12" aria-hidden="true">
            <path d="M3.2 8.8L8.8 3.2M6.2 3.2h2.6v2.6" />
          </svg>
        </button>
      </div>
      <span class="titlebar-title">PeerDav</span>
    </header>

    <main class="shell">
    <header class="hero">
      <div class="hero-copy">
        <p class="eyebrow">{{ t("app.eyebrow") }}</p>
        <h1>{{ t("app.title") }}</h1>
        <p class="lede">{{ t("app.lede") }}</p>
      </div>
      <div class="hero-tools">
        <div
          class="language-switch"
          role="group"
          :aria-label="t('language.label')"
        >
          <button
            type="button"
            :class="{ active: locale === 'zh-CN' }"
            :aria-pressed="locale === 'zh-CN'"
            lang="zh-CN"
            @click="setLocale('zh-CN')"
          >
            {{ t("language.zh") }}
          </button>
          <button
            type="button"
            :class="{ active: locale === 'en' }"
            :aria-pressed="locale === 'en'"
            lang="en"
            @click="setLocale('en')"
          >
            {{ t("language.en") }}
          </button>
        </div>
        <div class="status" :class="statusClass">
          <span class="dot" aria-hidden="true"></span>
          <span>{{ statusText }}</span>
        </div>
      </div>
    </header>

    <el-alert
      v-if="errorMessage"
      class="alert alert-error"
      type="error"
      :title="errorMessage"
      :closable="false"
      show-icon
    />
    <el-alert
      v-if="notice"
      class="alert alert-ok"
      type="success"
      :title="notice"
      :closable="false"
      show-icon
    />

    <section v-if="view === 'loading'" class="card">
      <p>{{ t("vault.loading") }}</p>
    </section>

    <section v-else-if="view === 'initialize'" class="card">
      <h2>{{ t("vault.initializeTitle") }}</h2>
      <p>{{ t("vault.initializeBody") }}</p>
      <form class="stack" @submit.prevent="initializeVault">
        <el-steps :active="initStep - 1" simple>
          <el-step :title="t('vault.stepEncryption')" />
          <el-step :title="t('vault.stepAdmin')" />
        </el-steps>
        <p class="hint">{{ t("vault.passwordRule") }}</p>

        <template v-if="initStep === 1">
          <label>
            {{ t("vault.encryptionPassword") }}
            <el-input
              v-model="masterPassword"
              type="password"
              show-password
              minlength="6"
              autocomplete="new-password"
              required
            />
          </label>
          <label>
            {{ t("vault.confirmEncryptionPassword") }}
            <el-input
              v-model="masterPasswordConfirm"
              type="password"
              show-password
              minlength="6"
              autocomplete="new-password"
              required
            />
          </label>
          <p class="hint">{{ t("vault.encryptionHint") }}</p>
          <div class="actions">
            <el-button type="primary" @click="goToAdminStep">
              {{ t("vault.nextStep") }}
            </el-button>
          </div>
        </template>

        <template v-else>
          <label>
            {{ t("vault.adminUsername") }}
            <el-input
              v-model="initAdminUsername"
              type="text"
              autocomplete="username"
              placeholder="admin"
              maxlength="64"
              required
            />
          </label>
          <label>
            {{ t("vault.adminPassword") }}
            <el-input
              v-model="initAdminPassword"
              type="password"
              show-password
              minlength="6"
              autocomplete="new-password"
              required
            />
          </label>
          <label>
            {{ t("vault.confirmAdminPassword") }}
            <el-input
              v-model="initAdminPasswordConfirm"
              type="password"
              show-password
              minlength="6"
              autocomplete="new-password"
              required
            />
          </label>
          <div class="actions">
            <el-button plain @click="backToEncryptionStep">
              {{ t("vault.previousStep") }}
            </el-button>
            <el-button type="primary" native-type="submit" :disabled="busy">
              {{ t("vault.initializeSubmit") }}
            </el-button>
          </div>
        </template>
      </form>
    </section>

    <section v-else-if="view === 'unlock'" class="card">
      <h2>{{ t("vault.unlockTitle") }}</h2>
      <p>{{ t("vault.unlockBody") }}</p>
      <form class="stack" @submit.prevent="unlockVault">
        <label>
          {{ t("vault.masterPassword") }}
          <el-input
            v-model="unlockPassword"
            type="password"
            show-password
            autocomplete="current-password"
            required
          />
        </label>
        <el-button type="primary" native-type="submit" :disabled="busy">
          {{ t("vault.unlockSubmit") }}
        </el-button>
      </form>
    </section>

    <section v-else-if="view === 'login'" class="card">
      <h2>{{ t("login.title") }}</h2>
      <p>{{ t("login.body") }}</p>
      <form class="stack" @submit.prevent="loginAdmin">
        <label>
          {{ t("login.username") }}
          <el-input
            v-model="loginUsername"
            type="text"
            autocomplete="username"
            required
          />
        </label>
        <label>
          {{ t("login.password") }}
          <el-input
            v-model="loginPassword"
            type="password"
            show-password
            autocomplete="current-password"
            required
          />
        </label>
        <el-button type="primary" native-type="submit" :disabled="busy">
          {{ t("login.submit") }}
        </el-button>
      </form>
    </section>

    <section v-else-if="view === 'change-admin'" class="card">
      <h2>{{ t("login.changeTitle") }}</h2>
      <p>{{ t("login.changeBody") }}</p>
      <form class="stack" @submit.prevent="changeAdminPassword">
        <label>
          {{ t("login.username") }}
          <el-input v-model="adminUsername" type="text" disabled />
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
          <el-button plain @click="backToLogin">
            {{ t("login.back") }}
          </el-button>
        </div>
      </form>
    </section>

    <section v-else class="card">
      <h2>{{ t("signedIn.title") }}</h2>
      <p>
        {{ t("signedIn.current") }}:
        <strong class="mono">{{ adminUsername }}</strong>
      </p>
      <div class="actions">
        <el-button plain :disabled="busy" @click="lockVault">
          {{ t("vault.lock") }}
        </el-button>
        <el-button plain @click="showMasterPasswordForm = !showMasterPasswordForm">
          {{ t("vault.changeMaster") }}
        </el-button>
      </div>

      <form
        v-if="showMasterPasswordForm"
        class="stack inset-form"
        @submit.prevent="changeMasterPassword"
      >
        <label>
          {{ t("vault.currentMaster") }}
          <el-input
            v-model="currentMasterPassword"
            type="password"
            show-password
            autocomplete="current-password"
            required
          />
        </label>
        <label>
          {{ t("vault.newMaster") }}
          <el-input
            v-model="nextMasterPassword"
            type="password"
            show-password
            minlength="6"
            autocomplete="new-password"
            required
          />
        </label>
        <label>
          {{ t("vault.confirmMaster") }}
          <el-input
            v-model="nextMasterPasswordConfirm"
            type="password"
            show-password
            minlength="6"
            autocomplete="new-password"
            required
          />
        </label>
        <el-button type="primary" native-type="submit" :disabled="busy">
          {{ t("vault.updateMaster") }}
        </el-button>
      </form>

      <div class="server-panel">
        <h3>{{ t("server.title") }}</h3>
        <p class="hint">{{ t("server.verifyHint") }}</p>
        <div class="grid">
          <label>
            {{ t("server.adminPassword") }}
            <el-input
              v-model="serverPassword"
              type="password"
              show-password
              autocomplete="current-password"
            />
          </label>
          <label>
            {{ t("server.bindAddress") }}
            <el-input v-model="serverBind" type="text" placeholder="0.0.0.0" />
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
            <el-input v-model="serverName" type="text" placeholder="PeerDav" />
          </label>
        </div>
        <div class="actions">
          <el-button
            type="primary"
            :disabled="busy || serverStatus.running"
            @click="startServer"
          >
            {{ t("server.start") }}
          </el-button>
          <el-button
            plain
            :disabled="busy || !serverStatus.running"
            @click="stopServer"
          >
            {{ t("server.stop") }}
          </el-button>
          <el-button plain :disabled="busy" @click="refreshAdminData">
            {{ t("server.refresh") }}
          </el-button>
        </div>
        <p class="hint">
          {{ t("server.status") }}: {{ serverStatusText }} ·
          {{
            t("server.summary", {
              shares: serverStatus.shareCount,
              pending: serverStatus.pendingCount,
              discovery: serverDiscoveryText,
            })
          }}
        </p>
      </div>

      <div class="server-panel">
        <h3>{{ t("shares.title") }}</h3>
        <p v-if="editingShareId" class="hint">
          {{ t("shares.editing", { name: shareName }) }}
        </p>
        <form class="stack" @submit.prevent="saveShare">
          <div class="grid">
            <label>
              {{ t("shares.name") }}
              <el-input
                v-model="shareName"
                type="text"
                :disabled="Boolean(editingShareId)"
                required
              />
            </label>
            <label>
              {{ t("shares.localDir") }}
              <el-input
                v-model="shareRoot"
                type="text"
                :disabled="Boolean(editingShareId)"
                required
              />
            </label>
          </div>
          <label>
            {{ t("shares.allowRules") }}
            <el-input
              v-model="shareAllow"
              type="text"
              :placeholder="t('shares.allowPlaceholder')"
            />
          </label>
          <label>
            {{ t("shares.denyRules") }}
            <el-input
              v-model="shareDeny"
              type="text"
              :placeholder="t('shares.denyPlaceholder')"
            />
          </label>
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

        <ul class="list">
          <li v-for="share in shares" :key="share.id">
            <div>
              <strong>{{ share.name }}</strong>
              <div class="hint mono">{{ share.rootPath }}</div>
              <div class="hint">{{ describePermissions(share.permissions) }}</div>
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
      </div>

      <div class="server-panel">
        <h3>{{ t("pending.title") }}</h3>
        <p class="hint">{{ t("pending.hint") }}</p>
        <ul class="list">
          <li v-for="request in pendingRequests" :key="request.id">
            <div>
              <strong class="mono">{{ request.peerIp }}</strong>
              <div class="hint">
                {{ request.shareName }} · {{ request.requestedPath }} ·
                {{ t("pending.attempts", { count: request.attempts }) }}
              </div>
            </div>
            <el-button type="primary" :disabled="busy" @click="approveRequest(request)">
              {{ t("pending.approve") }}
            </el-button>
          </li>
          <li v-if="!pendingRequests.length" class="hint">
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
            <el-button plain :disabled="busy" @click="revokeAuthorization(authorization)">
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
          <el-button :disabled="busy || !peerUrl || !peerCode" @click="connectPeer">
            {{ t("client.connect") }}
          </el-button>
          <el-button plain :disabled="busy" @click="run(refreshPeers)">
            {{ t("client.refreshSaved") }}
          </el-button>
        </div>
        <p v-if="peerNotice" class="hint">{{ peerNotice }}</p>

        <h3>{{ t("client.savedTitle") }}</h3>
        <ul class="list">
          <li v-for="peer in peers" :key="peer.id">
            <div>
              <strong>{{ peer.label }}</strong>
              <div class="hint mono">{{ peer.url }}</div>
              <div v-if="peerHealth[peer.id]" class="hint">
                {{ healthText(peerHealth[peer.id]) }}
              </div>
            </div>
            <div class="actions">
              <el-button type="primary" :disabled="busy" @click="openPeer(peer)">
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
          <li v-if="!peers.length" class="hint">{{ t("client.empty") }}</li>
        </ul>

        <div v-if="activePeerId">
          <h3>{{ t("peer.shares") }}</h3>
          <div class="actions">
            <el-button
              v-for="share in peerShares"
              :key="share.id"
              plain
              :disabled="busy"
              @click="openPeerShare(share.id)"
            >
              {{ share.name }}
            </el-button>
          </div>

          <div v-if="peerListing">
            <p class="hint mono">{{ t("peer.path", { path: peerListing.path }) }}</p>
            <div class="actions">
              <el-button plain :disabled="busy" @click="browsePeer(peerParentPath)">
                {{ t("peer.up") }}
              </el-button>
              <el-button plain :disabled="busy" @click="browsePeer('/')">
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
                            contentType:
                              entry.contentType || t("peer.unknownType"),
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
                    @click="enterPeerEntry(entry)"
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
                  <el-option
                    value="overwrite"
                    :label="t('peer.conflictOverwrite')"
                  />
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
      </div>
    </section>

    <footer class="footer">{{ t("app.footer") }}</footer>
    </main>

    <el-dialog
      v-model="showQuitDialog"
      :title="t('quit.title')"
      width="360"
      :close-on-click-modal="false"
      :close-on-press-escape="false"
    >
      <p class="quit-body">{{ t("quit.body") }}</p>
      <template #footer>
        <el-button plain @click="showQuitDialog = false">
          {{ t("quit.cancel") }}
        </el-button>
        <el-button type="danger" @click="confirmQuit">
          {{ t("quit.confirm") }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.window-frame {
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-radius: 15px;
  border: 1px solid rgba(22, 32, 46, 0.08);
  background:
    radial-gradient(circle at top left, rgba(47, 111, 237, 0.1), transparent 34%),
    radial-gradient(circle at bottom right, rgba(23, 163, 92, 0.08), transparent 32%),
    #f3f6fa;
  box-shadow: 0 18px 48px rgba(22, 32, 46, 0.18);
}

.titlebar {
  position: relative;
  flex: none;
  height: 42px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  padding: 0 14px;
  background: rgba(255, 255, 255, 0.6);
  border-bottom: 1px solid rgba(227, 233, 240, 0.9);
  backdrop-filter: blur(14px);
  user-select: none;
}

.traffic-lights {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
}

.traffic-lights--windows {
  margin-left: auto;
}

.traffic-lights--windows .traffic-light.close {
  order: 2;
}

.traffic-lights--windows .traffic-light.minimize {
  order: 0;
}

.traffic-lights--windows .traffic-light.maximize {
  order: 1;
}

.traffic-light {
  width: 13px;
  height: 13px;
  border: 0;
  border-radius: 50%;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  -webkit-app-region: no-drag;
}

.traffic-light svg {
  width: 9px;
  height: 9px;
  fill: none;
  stroke: rgba(0, 0, 0, 0.55);
  stroke-width: 1.4;
  stroke-linecap: round;
  stroke-linejoin: round;
  opacity: 0;
  transition: opacity 0.15s ease;
}

.traffic-lights:hover .traffic-light svg {
  opacity: 1;
}

.traffic-light.close {
  background: #ef6051;
}

.traffic-light.minimize {
  background: #f4b730;
}

.traffic-light.maximize {
  background: #4bbd38;
}

.titlebar-title {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
  font-size: 12px;
  line-height: 1;
  font-weight: 600;
  color: #6b7a90;
  letter-spacing: 0.04em;
}

.shell {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  width: 100%;
  padding: 32px max(24px, calc((100% - 720px) / 2)) 32px;
  display: flex;
  flex-direction: column;
  gap: 18px;
  box-sizing: border-box;
}

.hero {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 24px;
}

.eyebrow {
  margin: 0 0 8px;
  font-size: 12px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  color: #6b7a90;
}

h1 {
  margin: 0 0 12px;
  font-size: 30px;
  line-height: 1.2;
  color: #16202e;
}

.lede {
  margin: 0;
  color: #5a6879;
  line-height: 1.6;
  font-size: 14px;
}

.hero-tools {
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 10px;
}

.language-switch {
  display: inline-flex;
  gap: 2px;
  padding: 3px;
  border-radius: 999px;
  background: #eef2f7;
}

.language-switch button {
  border: 0;
  padding: 5px 10px;
  border-radius: 999px;
  font-family: inherit;
  font-size: 12px;
  font-weight: 600;
  color: #526072;
  background: transparent;
  cursor: pointer;
}

.language-switch button:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.75);
  color: #16202e;
}

.language-switch button.active {
  background: #ffffff;
  color: #16202e;
  box-shadow: 0 1px 3px rgba(22, 32, 46, 0.12);
}

.language-switch button:focus-visible {
  outline: 2px solid #2f6fed;
  outline-offset: 1px;
}

.status {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
  background: #eef2f7;
  color: #526072;
  white-space: nowrap;
}

.status .dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #8b9aac;
}

.status-new {
  background: #fff4e5;
  color: #9a5b00;
}

.status-new .dot {
  background: #e08a00;
}

.status-locked {
  background: #eef2f7;
  color: #526072;
}

.status-locked .dot {
  background: #728197;
}

.status-unlocked {
  background: #e6f7ee;
  color: #137a45;
}

.status-unlocked .dot {
  background: #17a35c;
}

.card {
  background: #ffffff;
  border: 1px solid #e3e9f0;
  border-radius: 16px;
  padding: 24px;
  box-shadow: 0 14px 40px rgba(22, 32, 46, 0.06);
}

.card h2 {
  margin: 0 0 12px;
  font-size: 19px;
  color: #16202e;
}

.card p {
  margin: 0 0 18px;
  color: #5a6879;
  line-height: 1.6;
  font-size: 14px;
}

.warning {
  color: #925c00 !important;
  font-weight: 600;
}

.mono {
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
}

.stack {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.stack > label,
.grid > label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 13px;
  font-weight: 600;
  color: #334155;
}

.preview-image {
  max-width: 100%;
  max-height: 360px;
  border-radius: 8px;
  border: 1px solid #e3e9f0;
  background: #ffffff;
  object-fit: contain;
}

.preview-text {
  margin: 0;
  max-height: 320px;
  overflow: auto;
  padding: 12px;
  border-radius: 8px;
  background: #f7f9fc;
  border: 1px solid #e3e9f0;
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-word;
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.inset-form {
  margin-top: 20px;
  padding: 18px;
  border-radius: 12px;
  background: #f7f9fc;
  border: 1px solid #e3e9f0;
}

.server-panel {
  margin-top: 26px;
  padding-top: 22px;
  border-top: 1px solid #e3e9f0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.server-panel h3 {
  margin: 0;
  font-size: 16px;
  color: #16202e;
}

.hint {
  margin: 0;
  color: #5a6879;
  font-size: 13px;
  line-height: 1.5;
}

.quit-body {
  margin: 0;
  color: #5a6879;
  font-size: 14px;
  line-height: 1.6;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
  gap: 14px;
}

.checks {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 16px;
}

.grid :deep(.el-input-number),
.grid :deep(.el-select) {
  width: 100%;
}

.list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.list li {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  padding: 12px;
  border: 1px solid #e3e9f0;
  border-radius: 10px;
  background: #fbfcfe;
}

.code-box {
  padding: 14px;
  border-radius: 12px;
  background: #fffaf0;
  border: 1px solid #f0c774;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.code {
  font-size: 14px;
  word-break: break-all;
  color: #3a2a10;
}

.footer {
  text-align: center;
  font-size: 12px;
  color: #8b98a9;
  padding-top: 8px;
}

@media (max-width: 560px) {
  .shell {
    padding: 28px 16px 24px;
  }

  .hero {
    flex-direction: column;
  }

  .hero-tools {
    align-items: flex-start;
  }

  h1 {
    font-size: 25px;
  }

  .card {
    padding: 20px;
  }
}
</style>

<style>
:root {
  --el-color-primary: #2f6fed;
  --el-color-primary-light-3: #6d9af2;
  --el-color-primary-light-5: #97b7f6;
  --el-color-primary-light-7: #c1d4fa;
  --el-color-primary-light-8: #d5e2fb;
  --el-color-primary-light-9: #eaf0fd;
  --el-color-primary-dark-2: #2659be;
  --el-border-radius-base: 10px;
  --el-component-size: 38px;
  font-family:
    Inter, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", Avenir,
    Helvetica, Arial, sans-serif;
  color: #16202e;
  background: transparent;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

html,
body,
#app {
  height: 100%;
  margin: 0;
  background: transparent;
  overflow: hidden;
}
</style>