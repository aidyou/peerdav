<script setup>
import { computed, onMounted, onUnmounted, provide, reactive, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ElMessage } from "element-plus";

import { errorStrategy, invoke, normalizeAppError, toUserMessage } from "./lib/invoke";
import { confirmAction } from "./lib/confirm";
import { stableList } from "./lib/listOrder";
import { generalSettings } from "./lib/general";
import { locale, setLocale, t } from "./lib/i18n";
import { resolvedTheme, setTheme, theme } from "./lib/theme";
import { SUPPORTED_ACCENTS, accent, setAccent } from "./lib/accent";
import {
  markSettingsSeen,
  shouldOpenSettingsOnLogin,
} from "./lib/workspace";
import { readRoute, writeRoute } from "./lib/navigation";
import { APP_CONTEXT } from "./lib/appContext";
import AppRail from "./components/AppRail.vue";
import AuthView from "./components/AuthView.vue";
import HomeView from "./components/HomeView.vue";
import PeerBrowser from "./components/PeerBrowser.vue";
import SettingsView from "./components/SettingsView.vue";
import TitleBarMenu from "./components/TitleBarMenu.vue";

const loading = ref(true);
const sessionResolved = ref(false);
const status = ref({
  initialized: false,
  unlocked: false,
  keyFile: null,
  keyError: null,
});
const showQuitDialog = ref(false);
const isMacOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
const isMaximized = ref(false);

const languageItems = computed(() => [
  { value: "zh-CN", label: t("language.zh"), lang: "zh-CN" },
  { value: "en", label: t("language.en"), lang: "en" },
]);

const themeItems = computed(() => [
  { value: "system", label: t("theme.system") },
  { value: "light", label: t("theme.light") },
  { value: "dark", label: t("theme.dark") },
]);

const accentItems = computed(() =>
  SUPPORTED_ACCENTS.map((value) => ({ value, label: t(`accent.${value}`) })),
);

const loggedIn = ref(false);
const adminUsername = ref("");
const mustChangeAdminPassword = ref(false);
const serverPassword = ref("");

const serverStatus = ref({
  running: false,
  bindAddress: null,
  discoveryRunning: false,
  discoveryPort: null,
  shareCount: 0,
  pendingCount: 0,
});
const pendingRequests = ref([]);
// 出站访问申请由后端内存保存（access_requests 命令）。视图重挂载或 webview 重载后
// 都从这份数据渲染，列表状态不会退回「可以申请」。
const accessRequests = ref([]);
const inboundConnections = ref([]);
const outboundConnections = ref([]);
const deviceFingerprint = ref("");
let connectionRefreshInFlight = false;
// 设置页的管理数据（共享 / 授权）也放在共享上下文：重新进入设置页时先用旧数据
// 渲染，后台刷新再替换，避免每次进入都闪一次骨架屏。
const adminShares = ref([]);
const adminAuthorizations = ref([]);
const adminLoading = ref(true);
// 每台已连接服务器的实时共享数。放在共享上下文里，首页重挂载时不会先掉成「—」。
const peerShareCounts = ref({});
const peers = ref([]);
// 首屏数据到达前列表要显示骨架屏；标记放在共享上下文里，首页与设置页读同一份，
// 避免两个视图各自维护一份可能不同步的加载状态。
const peersLoading = ref(true);
const onlinePeers = computed(() => peers.value.filter((peer) => peer.online));
const discovered = ref([]);
const discoveredLoading = ref(true);
const activePeerId = ref("");
const activePeerSnapshot = ref(null);
const workspaceView = ref("home");
let lastInterfaceActivity = Date.now();
const interfaceLocked = ref(false);
const unlockPassword = ref("");
const unlockingInterface = ref(false);

async function lockInterface() {
  if (!loggedIn.value || interfaceLocked.value) return;
  try {
    await invoke("lock_interface");
    interfaceLocked.value = true;
    unlockPassword.value = "";
  } catch (error) { handleFailure(error); }
}

async function unlockInterface() {
  if (unlockingInterface.value || !unlockPassword.value) return;
  unlockingInterface.value = true;
  try {
    await invoke("unlock_interface", { password: unlockPassword.value });
    interfaceLocked.value = false;
    lastInterfaceActivity = Date.now();
    void refreshPeers();
    void refreshConnections();
    void invoke("refresh_tray_menu").catch(handleFailure);
  } catch (error) { setRawError(error); }
  finally { unlockPassword.value = ""; unlockingInterface.value = false; }
}

const settingsTab = ref("general");
const transfers = ref({});

let eventUnlisten = [];
let activeToast = null;
let statusRequestInFlight = false;

function showToast(message, type, duration) {
  activeToast?.close();
  activeToast = ElMessage({
    message,
    type,
    duration,
    showClose: true,
    offset: 54,
  });
}

function setError(key, params) {
  showToast(t(key, params), "error", 5000);
}

function setRawError(error) {
  showToast(toUserMessage(error), "error", 5000);
}

function setInfo(key, params) {
  showToast(t(key, params), "info", 5000);
}

function setNotice(key, params) {
  showToast(t(key, params), "success", 3000);
}

function clearMessages() {
  activeToast?.close();
  activeToast = null;
}

const view = computed(() => {
  if (loading.value) return "loading";
  if (!status.value.initialized) return "initialize";
  if (!status.value.unlocked) return "unlock";
  if (!sessionResolved.value) return "loading";
  if (mustChangeAdminPassword.value) return "change-admin";
  if (!loggedIn.value) return "login";
  return "ready";
});

const activePeer = computed(
  () =>
    peers.value.find((peer) => peer.id === activePeerId.value) ??
    (activePeerSnapshot.value?.id === activePeerId.value
      ? activePeerSnapshot.value
      : null),
);

function setTransferProgress(payload, register = false) {
  if (!payload?.transferId) return;
  if (!register && payload.direction === "upload" && !(payload.transferId in transfers.value)) {
    return;
  }
  transfers.value = {
    ...transfers.value,
    [payload.transferId]: { ...payload, updatedAt: Date.now() },
  };
}

function clearTransfer(transferId) {
  if (!transferId || !(transferId in transfers.value)) return;
  const next = { ...transfers.value };
  delete next[transferId];
  transfers.value = next;
}

onMounted(async () => {
  void bindWindowState();
  const configureShortcuts = async () => {
    if (interfaceLocked.value) return;
    try {
      await invoke("configure_shortcuts", { toggle: generalSettings.toggleShortcut, lock: generalSettings.lockShortcut });
    } catch (error) { handleFailure(error); }
  };
  void configureShortcuts();
  eventUnlisten.push(watch(() => [generalSettings.toggleShortcut, generalSettings.lockShortcut], configureShortcuts));
  eventUnlisten.push(await listen("app://interface-locked", () => { if (loggedIn.value) interfaceLocked.value = true; }));
  eventUnlisten.push(await listen("app://navigate", ({ payload }) => {
    if (interfaceLocked.value) return;
    if (payload === "home") openHome();
    else if (payload === "settings") openSettings();
    else if (typeof payload === "string" && payload.startsWith("peer/")) {
      const peer = peers.value.find((entry) => entry.id === payload.slice(5));
      if (peer) openPeer(peer);
    }
  }));
  eventUnlisten.push(watch(() => peers.value.map(({ id, label }) => `${id}:${label}`).join("|"), async () => {
    if (interfaceLocked.value) return;
    try { await invoke("refresh_tray_menu"); } catch (error) { console.warn("tray menu refresh failed", error); }
  }));
  const recordActivity = (event) => { if (event.isTrusted) lastInterfaceActivity = Date.now(); };
  for (const name of ["pointerdown", "keydown", "wheel", "touchstart"]) {
    window.addEventListener(name, recordActivity, { passive: true });
    eventUnlisten.push(() => window.removeEventListener(name, recordActivity));
  }
  const idleTimer = setInterval(() => {
    if (!interfaceLocked.value && loggedIn.value && generalSettings.autoLock && Date.now() - lastInterfaceActivity >= generalSettings.idleMinutes * 60000) {
      void lockInterface();
    }
  }, 1000);
  eventUnlisten.push(() => clearInterval(idleTimer));
  window.addEventListener("hashchange", handleHashChange);
  eventUnlisten.push(() =>
    window.removeEventListener("hashchange", handleHashChange),
  );
  await bindServerEvents();
  await refreshStatus();
  // 服务器列表与发现快照互相独立，也不该挡在管理员会话恢复前面：一次慢刷新只会
  // 让首屏多显示一会儿骨架屏，而不是把登录界面拖出来。两者内部都已捕获错误。
  void refreshPeers();
  void refreshDiscovered();
  await restoreAdminSession();
  await refreshServerStatus();
  await resumeServer();
  void syncPeers();
  // 上次运行留下的「等待批准」申请：后端还记着，前端重新接上等待即可。
  if (status.value.unlocked) void resumeAccessRequests();
  void refreshConnections();
  const connectionTimer = setInterval(() => {
    if (status.value.unlocked && !interfaceLocked.value) void refreshConnections();
  }, 3000);
  eventUnlisten.push(() => clearInterval(connectionTimer));
});

onUnmounted(() => {
  for (const unlisten of eventUnlisten) {
    unlisten();
  }
  eventUnlisten = [];
  activeToast?.close();
  activeToast = null;
});

/// Tracks the OS window's maximized state so the rounded transparent frame can
/// drop its corner radius when the window fills the screen. A maximized window
/// keeps its square bounds, so the rounded corners would otherwise expose the
/// transparent gap at each screen edge. The resize listener and its initial read
/// are wrapped in try/catch: outside a Tauri runtime (e.g. tests) these window
/// APIs are unavailable and the frame simply keeps its normal rounded state.
async function bindWindowState() {
  const win = getCurrentWindow();
  try {
    isMaximized.value = await win.isMaximized();
  } catch (error) {
    console.warn("PeerDav window maximize state is unavailable", error);
    return;
  }
  try {
    const unlisten = await win.onResized(() => {
      void win
        .isMaximized()
        .then((value) => {
          isMaximized.value = value;
        })
        .catch((error) =>
          console.warn("PeerDav maximize state refresh failed", error),
        );
    });
    eventUnlisten.push(unlisten);
  } catch (error) {
    console.warn("PeerDav window resize listener is unavailable", error);
  }
}

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
        void refreshPendingRequests();
        setNotice("pending.newRequest", { ip: request.peerIp });
      }),
    );
    eventUnlisten.push(
      await listen("app://pending-canceled", (event) => {
        const request = event.payload;
        if (!request?.id) return;
        void refreshPendingRequests();
        setNotice("pending.canceled", { ip: request.peerIp });
      }),
    );
    eventUnlisten.push(
      await listen("app://peer-revoked", (event) => {
        const peer = event.payload;
        if (!peer?.id) return;
        if (activePeerId.value === peer.id) {
          closePeer();
        }
        const noticeKey =
          peer.reason === "revoked"
            ? "peer.revoked"
            : peer.reason === "bound-elsewhere"
              ? "peer.boundElsewhere"
              : "peer.removed";
        setNotice(noticeKey, { label: peer.label ?? "" });
        void refreshPeers();
      }),
    );
    eventUnlisten.push(
      await listen("app://transfer-progress", (event) => {
        setTransferProgress(event.payload);
      }),
    );
    eventUnlisten.push(
      await listen("app://error", (event) => {
        setRawError(event.payload);
      }),
    );
    eventUnlisten.push(
      await listen("app://discovered-peers", (event) => {
        discovered.value = stableList(event.payload, (entry) => entry.serverId ?? entry.url);
      }),
    );
    eventUnlisten.push(
      await listen("app://peer-status", (event) => {
        const status = event.payload;
        if (!status?.id) return;
        peers.value = peers.value.map((peer) =>
          peer.id === status.id
            ? { ...peer, online: Boolean(status.online) }
            : peer,
        );
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

/// Applies a route parsed from the URL hash. A peer that is not in the saved
/// catalog still opens the page shell so the unavailable state is explicit
/// instead of silently redirecting to Home.
function applyRoute(route) {
  activePeerSnapshot.value = null;
  if (route.view === "peer" && route.peerId) {
    activePeerId.value = route.peerId;
    activePeerSnapshot.value =
      peers.value.find((peer) => peer.id === route.peerId) ?? null;
    workspaceView.value = "peer";
    return;
  }
  if (route.view === "settings") {
    activePeerId.value = "";
    settingsTab.value = route.settingsTab;
    workspaceView.value = "settings";
    return;
  }
  activePeerId.value = "";
  settingsTab.value = "general";
  workspaceView.value = "home";
}

function syncRoute() {
  writeRoute({
    view: workspaceView.value,
    peerId: activePeerId.value,
    settingsTab: settingsTab.value,
  });
}

/// Back/forward navigation changes only the hash. Re-applying the route keeps
/// the visible page in sync without reloading the webview or resetting state.
function handleHashChange() {
  if (!loggedIn.value) return;
  applyRoute(readRoute());
  syncRoute();
}

/// Restores the in-memory admin session after a webview reload. The backend
/// keeps the session until the vault is locked or the admin logs out.
async function restoreAdminSession() {
  try {
    if (!status.value.unlocked) return;
    const session = await invoke("admin_session");
    if (session?.username) {
      adminUsername.value = session.username;
      interfaceLocked.value = Boolean(session.interfaceLocked);
      mustChangeAdminPassword.value = Boolean(session.mustChangePassword);
      loggedIn.value = !session.mustChangePassword;
      if (loggedIn.value) {
        applyRoute(readRoute());
        syncRoute();
      }
    }
  } catch (error) {
    console.warn("PeerDav admin session restore failed", error);
  } finally {
    // Unknown authentication state must never render the sign-in form.
    sessionResolved.value = true;
  }
}

function handleFailure(error) {
  if (interfaceLocked.value && error?.code === "AUTH_INTERFACE_LOCKED") return;
  setRawError(error);

  const strategy = errorStrategy(error);
  if (strategy === "interface-lock") {
    interfaceLocked.value = true;
  } else if (strategy === "unlock") {
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
    // A stale or transient peer lookup must not tear down an open browser.
    // Refresh the catalog; explicit removal events or the user's own unbind
    // action are the only paths that leave the current peer view.
    void refreshPeers();
  }
}

function landAfterLogin() {
  activePeerId.value = "";
  activePeerSnapshot.value = null;
  if (shouldOpenSettingsOnLogin()) {
    markSettingsSeen();
    settingsTab.value = "general";
    workspaceView.value = "settings";
    syncRoute();
  } else {
    applyRoute(readRoute());
    syncRoute();
  }
}

function completeLogin(result, password) {
  adminUsername.value = result.username;
  mustChangeAdminPassword.value = Boolean(result.mustChangePassword);
  loggedIn.value = !result.mustChangePassword;
  // Kept in memory for management commands during this session only.
  serverPassword.value = password;
  if (loggedIn.value) {
    landAfterLogin();
    // 服务可能在本机上次运行时就已经收到申请：登录后立刻拉一次，首页控台才不会漏掉
    // 那些在打开应用之前就排队的请求（它们不会再触发一次 pending 事件）。
    void refreshPendingRequests();
    // 登录后同样要接管上次留下的等待中申请。
    void resumeAccessRequests();
  }
}

function completeAdminPassword(result, newPassword) {
  const wasForced = mustChangeAdminPassword.value;
  mustChangeAdminPassword.value = result.mustChangePassword;
  loggedIn.value = true;
  serverPassword.value = newPassword;
  if (wasForced && !result.mustChangePassword) {
    landAfterLogin();
  }
}

function applyVaultStatus(next) {
  status.value = next;
  if (!next.unlocked) {
    resetAdminSession();
  }
}

function setAdminUsername(name) {
  adminUsername.value = name;
}

function resetAdminSession() {
  loggedIn.value = false;
  mustChangeAdminPassword.value = false;
  adminUsername.value = "";
  serverPassword.value = "";
  pendingRequests.value = [];
  activePeerId.value = "";
  activePeerSnapshot.value = null;
  settingsTab.value = "general";
  workspaceView.value = "home";
  syncRoute();
}

async function refreshServerStatus() {
  try {
    serverStatus.value = await invoke("server_status");
  } catch (error) {
    handleFailure(error);
  }
}

/* 审批入站申请在首页控台与设置页「客户端」里是同一个动作：入口放在共享上下文，
   避免两处各自拼 admin 参数、各自刷新列表而出现短暂不一致。 */
let adminRefreshVersion = 0;
async function refreshAdminData() {
  const version = ++adminRefreshVersion;
  adminLoading.value = true;
  try {
    const args = { username: adminUsername.value, password: serverPassword.value };
    const [shares, pending, authorizations] = await Promise.all([
      invoke("list_shares", args),
      invoke("list_pending_requests", args),
      invoke("list_authorizations", args),
    ]);
    if (version !== adminRefreshVersion) return;
    adminShares.value = stableList(shares, (entry) => entry.id);
    pendingRequests.value = stableList(pending, (entry) => entry.machineId ?? entry.id);
    adminAuthorizations.value = stableList(authorizations, (entry) => entry.machineId ?? entry.id);
    await refreshServerStatus();
  } finally {
    if (version === adminRefreshVersion) adminLoading.value = false;
  }
}

async function refreshPendingRequests() {
  try {
    await refreshAdminData();
  } catch (error) {
    handleFailure(error);
  }
}

async function approvePendingRequest(requestId) {
  const result = await invoke("approve_pending_request", {
    username: adminUsername.value,
    password: serverPassword.value,
    requestId,
  });
  setNotice(result.autoDelivery ? "pending.approvedAuto" : "pending.approved");
  // 审批完立即从待审批列表里消失：首页控台和设置页读的是同一份列表。
  await refreshPendingRequests();
  return result;
}

/* 出站访问申请：状态存在后端内存里，前端只负责展示与接线。
   这样切换页面（视图重建）或 webview 重载都不会让「等待批准」退回「可以申请」。 */

/// 正在等待批准、已经接上长轮询的地址，避免重复发起等待。
const accessWaits = new Set();

async function refreshAccessRequests() {
  try {
    const entries = await invoke("access_requests");
    accessRequests.value = Array.isArray(entries) ? entries : [];
  } catch (error) {
    const normalized = normalizeAppError(error);
    console.error(
      `[peerdav:${normalized.code}] access request snapshot failed`,
      { detail: normalized.detail, context: normalized.context },
    );
  }
}

function accessRequestFor(url) {
  return accessRequests.value.find((entry) => entry.url === url) ?? null;
}

/// 等待审批：成功后后端已保存对端，这里只刷新列表；失败原因留在申请状态里，
/// 列表直接显示，不需要额外的 toast。
function trackPeerAccess(url, label = null) {
  if (accessWaits.has(url)) return;
  accessWaits.add(url);
  void (async () => {
    try {
      const peer = await invoke("wait_for_peer_access", { url, label });
      await refreshPeers();
      setNotice("home.added", { label: peer.label });
    } catch (error) {
      const normalized = normalizeAppError(error);
      if (normalized.code === "PEER_ACCESS_CANCELED") {
        setNotice("home.requestCanceled");
      } else if (normalized.code === "VAULT_LOCKED") {
        // 金库锁着只是暂时接不上：后端保留「等待批准」，解锁后会重新接上。
        handleFailure(error);
      }
    } finally {
      accessWaits.delete(url);
      await refreshAccessRequests();
    }
  })();
}

/// 申请访问一台服务器。后端会复用同一地址仍在等待的申请，不会重复登记。
async function beginPeerAccess(url, label = null, fingerprint = null) {
  const args = { url, label };
  if (fingerprint) args.fingerprint = fingerprint;
  const entry = await invoke("request_peer_access", args);
  await refreshAccessRequests();
  if (entry.status === "waiting") {
    trackPeerAccess(url, label);
  } else if (entry.status === "accessible") {
    await refreshPeers();
  }
  return entry;
}

async function removeLocalPeer(peer) {
  if (!await confirmAction(t("client.removeLocalConfirm", { label: peer.label }), t("client.removeLocal"))) return;
  try {
    await invoke("remove_local_peer", { peerId: peer.id });
    if (activePeerId.value === peer.id) closePeer();
    await Promise.all([refreshPeers(), refreshAccessRequests()]);
    setNotice("client.removedLocal");
  } catch (error) { handleFailure(error); }
}

async function cancelPeerAccess(url) {
  await invoke("cancel_peer_access", { url });
  await refreshAccessRequests();
}

/// 启动/登录后恢复：后端记着哪些申请还在等批准，前端只需要重新接上等待。
async function resumeAccessRequests() {
  await refreshAccessRequests();
  for (const entry of accessRequests.value) {
    if (entry.status === "waiting") {
      trackPeerAccess(entry.url, entry.label ?? null);
    }
  }
}

/// Restores a server that the administrator left enabled on the previous run.
/// This runs even while the vault is locked: WebDAV only needs the non-secret
/// share/authorization catalog, not the vault key.
async function resumeServer() {
  try {
    serverStatus.value = await invoke("resume_server");
  } catch (error) {
    handleFailure(error);
  }
}

async function refreshConnections() {
  if (connectionRefreshInFlight) return;
  connectionRefreshInFlight = true;
  try {
    const [inbound, outbound, fingerprint] = await Promise.all([
      invoke("inbound_connections"), invoke("outbound_connections"), invoke("device_fingerprint"),
    ]);
    inboundConnections.value = Array.isArray(inbound) ? inbound : [];
    outboundConnections.value = Array.isArray(outbound) ? outbound : [];
    deviceFingerprint.value = typeof fingerprint === "string" ? fingerprint : "";
  } catch (error) {
    console.error("Connection snapshot refresh failed", normalizeAppError(error));
  } finally {
    connectionRefreshInFlight = false;
  }
}

async function refreshPeers() {
  peersLoading.value = true;
  try {
    peers.value = stableList(await invoke("list_peers"), (entry) => entry.fingerprint ?? entry.id);
  } catch (error) {
    const normalized = normalizeAppError(error);
    console.error(`[peerdav:${normalized.code}] saved peers refresh failed`, {
      detail: normalized.detail,
      context: normalized.context,
    });
  } finally {
    peersLoading.value = false;
  }
}

/// Reads the current passive discovery snapshot. Live changes arrive through
/// the `app://discovered-peers` event, so views never trigger a LAN scan or a
/// page reload themselves.
async function refreshDiscovered() {
  discoveredLoading.value = true;
  try {
    const peersOnLan = await invoke("discovered_peers");
    discovered.value = stableList(peersOnLan, (entry) => entry.serverId ?? entry.url);
  } catch (error) {
    const normalized = normalizeAppError(error);
    console.error(
      `[peerdav:${normalized.code}] discovery snapshot failed`,
      {
        detail: normalized.detail,
        context: normalized.context,
      },
    );
  } finally {
    discoveredLoading.value = false;
  }
}

/// Reconciles saved peers with their server bindings. Backend polling repeats
/// this automatically; an immediate check keeps startup responsive to a server
/// that was unbound while this application was closed.
async function syncPeers() {
  try {
    const removed = await invoke("sync_peers");
    if (removed > 0) {
      await refreshPeers();
    }
  } catch (error) {
    console.warn("PeerDav peer synchronization failed", error);
  }
}

function openPeer(peer) {
  activePeerId.value = peer.id;
  activePeerSnapshot.value = peer;
  workspaceView.value = "peer";
  syncRoute();
}

function closePeer() {
  openHome();
}

function openHome() {
  activePeerId.value = "";
  activePeerSnapshot.value = null;
  settingsTab.value = "general";
  workspaceView.value = "home";
  syncRoute();
}

function openSettings() {
  activePeerId.value = "";
  activePeerSnapshot.value = null;
  workspaceView.value = "settings";
  settingsTab.value = "general";
  syncRoute();
}

/// Settings tabs are part of the URL so a reload preserves the visible pane.
function selectSettingsTab(tab) {
  settingsTab.value = tab;
  if (workspaceView.value === "settings") syncRoute();
}

async function logout() {
  try {
    await invoke("admin_logout");
    resetAdminSession();
    setNotice("login.loggedOut");
  } catch (error) {
    handleFailure(error);
  }
}

provide(
  APP_CONTEXT,
  reactive({
    status,
    adminUsername,
    serverPassword,
    serverStatus,
    pendingRequests,
    accessRequests,
    inboundConnections,
    outboundConnections,
    deviceFingerprint,
    refreshConnections,
    accessRequestFor,
    beginPeerAccess,
    cancelPeerAccess,
    removeLocalPeer,
    resumeAccessRequests,
    refreshAccessRequests,
    adminShares,
    adminAuthorizations,
    adminLoading,
    peerShareCounts,
    peers,
    peersLoading,
    discovered,
    discoveredLoading,
    activePeerId,
    workspaceView,
    settingsTab,
    transfers,
    invoke,
    clearMessages,
    handleFailure,
    setError,
    setInfo,
    setRawError,
    setNotice,
    setTransferProgress,
    clearTransfer,
    refreshStatus,
    refreshServerStatus,
    refreshPendingRequests,
    refreshAdminData,
    approvePendingRequest,
    refreshPeers,
    refreshDiscovered,
    completeLogin,
    completeAdminPassword,
    applyVaultStatus,
    setAdminUsername,
    resetAdminSession,
    resumeServer,
    openPeer,
    closePeer,
    openHome,
    openSettings,
    selectSettingsTab,
    logout,
    lockInterface,
    interfaceLocked,
  }),
);
</script>

<template>
  <div
    class="window-frame"
    :class="{ 'is-maximized': isMaximized }"
    :style="{
      '--window-overlay-inset': isMaximized ? '0px' : '1px',
      '--window-overlay-radius': isMaximized ? '0px' : 'var(--window-inner-radius)',
    }"
  >
    <header
      class="titlebar"
      :class="{ 'titlebar--mac': isMacOS, 'titlebar--win': !isMacOS }"
      data-tauri-drag-region
    >
      <div id="titlebar-start" class="titlebar-slot titlebar-slot--start">
        <div
          class="traffic-lights"
          :class="{ 'traffic-lights--windows': !isMacOS }"
        >
          <el-tooltip
            :content="t('window.close')"
            placement="bottom"
            :show-after="200"
            :hide-after="0"
            :enterable="false"
          >
            <button
              type="button"
              class="traffic-light close"
              :aria-label="t('window.close')"
              @click="closeWindow"
            >
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path d="M3.2 3.2l5.6 5.6M8.8 3.2L3.2 8.8" />
              </svg>
            </button>
          </el-tooltip>
          <el-tooltip
            :content="t('window.minimize')"
            placement="bottom"
            :show-after="200"
            :hide-after="0"
            :enterable="false"
          >
            <button
              type="button"
              class="traffic-light minimize"
              :aria-label="t('window.minimize')"
              @click="minimizeWindow"
            >
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path d="M3 6h6" />
              </svg>
            </button>
          </el-tooltip>
          <el-tooltip
            :content="t('window.maximize')"
            placement="bottom"
            :show-after="200"
            :hide-after="0"
            :enterable="false"
          >
            <button
              type="button"
              class="traffic-light maximize"
              :aria-label="t('window.maximize')"
              @click="toggleMaximizeWindow"
            >
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path d="M3.2 8.8L8.8 3.2M6.2 3.2h2.6v2.6" />
              </svg>
            </button>
          </el-tooltip>
        </div>
      </div>

      <div
        :aria-hidden="interfaceLocked || undefined"
        :inert="interfaceLocked || undefined"
        v-show="!interfaceLocked"
        id="titlebar-center"
        class="titlebar-slot titlebar-slot--center"
      ></div>

      <div v-show="!interfaceLocked" id="titlebar-end" class="titlebar-slot titlebar-slot--end">
        <div class="titlebar-actions">
          <TitleBarMenu
            name="language"
            :aria-label="t('titlebar.language')"
            :model-value="locale"
            :items="languageItems"
            @update:model-value="setLocale"
          >
            <template #icon>
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path
                  class="titlebar-icon-fill"
                  d="m18.5 10l4.4 11h-2.155l-1.201-3h-4.09l-1.199 3h-2.154L16.5 10zM10 2v2h6v2h-1.968a18.2 18.2 0 0 1-3.62 6.301a15 15 0 0 0 2.335 1.707l-.75 1.878A17 17 0 0 1 9 13.725a16.7 16.7 0 0 1-6.201 3.548l-.536-1.929a14.7 14.7 0 0 0 5.327-3.042A18 18 0 0 1 4.767 8h2.24A16 16 0 0 0 9 10.877a16.2 16.2 0 0 0 2.91-4.876L2 6V4h6V2zm7.5 10.885L16.253 16h2.492z"
                />
              </svg>
            </template>
          </TitleBarMenu>
          <TitleBarMenu
            name="theme"
            :aria-label="t('titlebar.theme')"
            :model-value="theme"
            :items="themeItems"
            @update:model-value="setTheme"
          >
            <template #icon>
              <svg
                v-if="resolvedTheme === 'dark'"
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" />
              </svg>
              <svg
                v-else-if="resolvedTheme === 'light'"
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <circle cx="12" cy="12" r="4" />
                <path
                  d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19"
                />
              </svg>
              <svg v-else viewBox="0 0 24 24" aria-hidden="true">
                <circle cx="12" cy="12" r="9" />
                <path d="M12 3a9 9 0 0 1 0 18z" class="titlebar-icon-fill" />
              </svg>
            </template>
          </TitleBarMenu>
          <TitleBarMenu
            name="accent"
            :aria-label="t('titlebar.accent')"
            :model-value="accent"
            :items="accentItems"
            @update:model-value="setAccent"
          >
            <template #icon>
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M12 3a9 9 0 1 0 0 18c1.4 0 2-1 2-2 0-1.2-1-1.5-1-2.6 0-1 .8-1.4 1.8-1.4H17a4 4 0 0 0 4-4c0-4-4-8-9-8z"
                />
                <circle cx="8" cy="10" r="1" />
                <circle cx="12" cy="7.5" r="1" />
                <circle cx="16" cy="10" r="1" />
              </svg>
            </template>
          </TitleBarMenu>
        </div>
        <span class="titlebar-divider" aria-hidden="true"></span>
        <div id="titlebar-file-actions" class="titlebar-file-actions"></div>
      </div>
    </header>

    <main class="shell" :class="{ 'shell--workspace': view === 'ready' || view === 'loading' }">
      <form v-if="interfaceLocked" class="interface-lock" @submit.prevent="unlockInterface">
        <h1>{{ t("lock.title") }}</h1>
        <p>{{ t("lock.hint") }}</p>
        <el-input v-model="unlockPassword" type="password" show-password autocomplete="current-password" :placeholder="t('vault.adminPassword')" :disabled="unlockingInterface" />
        <el-button type="primary" native-type="submit" :loading="unlockingInterface">{{ t("lock.unlock") }}</el-button>
      </form>
      <AuthView v-else-if="view !== 'ready'" :view="view" />

      <div v-else class="workspace">
        <AppRail
          :peers="onlinePeers"
          :active-peer-id="activePeerId"
          :active-view="workspaceView"
          @open-home="openHome"
          @open-peer="openPeer"
          @open-settings="openSettings"
        />
        <div
          class="workspace-content"
          :class="{
            'workspace-content--browser': workspaceView === 'peer',
            'workspace-content--fluid': workspaceView === 'home',
          }"
        >
          <HomeView v-if="workspaceView === 'home'" />
          <PeerBrowser
            v-else-if="workspaceView === 'peer' && activePeer"
            :key="activePeer.id"
            :peer="activePeer"
          />
          <SettingsView v-else-if="workspaceView === 'settings'" />
          <div v-else-if="workspaceView === 'peer'" class="peer-unavailable">
            <p>{{ t("peer.unavailable") }}</p>
            <el-button type="primary" @click="openHome">
              {{ t("home.title") }}
            </el-button>
          </div>
          <HomeView v-else />
        </div>
      </div>

      <footer v-if="view !== 'ready' && view !== 'loading'" class="footer">
        {{ t("app.footer") }}
      </footer>
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
.interface-lock {
  width: min(360px, 90%);
  margin: auto;
  display: grid;
  gap: 16px;
}
.window-frame {
  box-sizing: border-box;
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-radius: var(--window-radius);
  border: 1px solid var(--app-frame-border);
  /* Keep the opaque fill even when the WebView cannot render color-mix(). */
  background-color: var(--app-bg-solid);
  background-image:
    radial-gradient(
      circle at top left,
      color-mix(in oklab, var(--accent) 12%, transparent),
      transparent 34%
    ),
    radial-gradient(
      circle at bottom right,
      color-mix(in oklab, var(--accent-green) 10%, transparent),
      transparent 32%
    );
  box-shadow: 0 18px 48px rgba(22, 32, 46, 0.18);
}

/* A maximized window fills the screen, so the rounded frame and its drop
   shadow no longer apply: keep the corners square and flush to the edges. */
.window-frame.is-maximized {
  border-radius: 0;
  border-color: transparent;
  box-shadow: none;
}

.window-frame.is-maximized .titlebar {
  border-radius: 0;
}

.titlebar {
  position: relative;
  z-index: 20;
  flex: none;
  height: var(--titlebar-height);
  box-sizing: border-box;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 14px;
  border-radius: var(--window-inner-radius) var(--window-inner-radius) 0 0;
  background: var(--titlebar-bg);
  border-bottom: 1px solid var(--titlebar-border);
  -webkit-backdrop-filter: blur(14px);
  backdrop-filter: blur(14px);
  user-select: none;
}

/* Three reusable regions: start, centre and end. The centre stays empty for
   now and simply absorbs the free width, so pages can mount content there
   later without changing the platform-specific ordering below. */
.titlebar-slot {
  flex: none;
  min-width: 0;
  display: flex;
  align-items: center;
}

.titlebar-slot--center {
  flex: 1 1 auto;
}

.titlebar-slot--end {
  gap: 0;
}

/* macOS: window controls start the bar; file actions and appearance controls
   end it, with appearance anchored at the outer edge. */
.titlebar--mac .titlebar-slot--start {
  order: 0;
}

.titlebar--mac .titlebar-slot--center {
  order: 1;
}

.titlebar--mac .titlebar-slot--end {
  order: 2;
}

.titlebar--mac .titlebar-file-actions {
  order: 0;
}

.titlebar--mac .titlebar-slot--end > .titlebar-divider {
  order: 1;
}

.titlebar--mac .titlebar-actions {
  order: 2;
}

/* Windows / Linux: app controls start the bar; window controls end it. The
   file actions stay next to the appearance group, far from the window
   buttons, so the two never compete for the same edge. */
.titlebar--win .titlebar-slot--end {
  order: 0;
}

.titlebar--win .titlebar-slot--center {
  order: 1;
}

.titlebar--win .titlebar-slot--start {
  order: 2;
}

.titlebar--win .titlebar-actions {
  order: 0;
}

.titlebar--win .titlebar-slot--end > .titlebar-divider {
  order: 1;
}

.titlebar--win .titlebar-file-actions {
  order: 2;
}

.traffic-lights {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
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

.titlebar-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 4px;
  -webkit-app-region: no-drag;
}

.titlebar-file-actions {
  flex: none;
  display: flex;
  align-items: center;
  -webkit-app-region: no-drag;
}

/* The slot is shared by every page; only the file browser fills it. */
.titlebar-file-actions:empty {
  display: none;
}

/* The separator only exists while a page owns the file-action slot. */
.titlebar-slot--end:has(.titlebar-file-actions:empty) > .titlebar-divider {
  display: none;
}

/* On Windows / Linux the appearance menu sits on the left, so its dropdown
   must open toward the content edge instead of running off-screen. */
.titlebar--win :deep(.titlebar-menu__list) {
  right: auto;
  left: 0;
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

.shell--workspace {
  padding: 0;
  gap: 0;
  overflow: hidden;
}

.workspace {
  flex: 1;
  min-height: 0;
  display: flex;
}

.workspace-content {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  padding: 24px max(20px, calc((100% - 1040px) / 2)) 24px;
  box-sizing: border-box;
}

/* Home is a browse surface, so it follows Apple's fluid desktop layout: content
   fills the available width and the server grid adds columns as the window
   grows. Settings keeps the centered reading width above. */
.workspace-content.workspace-content--fluid {
  padding: 24px clamp(20px, 4vw, 56px);
}

/* The browser owns its internal scrolling; this pane must not add a second
   scroll container around the fixed browser frame. */
.workspace-content.workspace-content--browser {
  position: relative;
  padding: 0;
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
}

.peer-unavailable {
  max-width: 460px;
  margin: 80px auto;
  padding: 0 24px;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
  text-align: center;
}

.peer-unavailable p {
  margin: 0;
  color: var(--app-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}

@media (max-width: 560px) {
  .shell {
    padding: 28px 16px 24px;
  }

  .shell--workspace {
    padding: 0;
  }

  .workspace-content {
    padding: 18px 14px;
  }
}
</style>

<style>
/* Shared vertical rule used between titlebar groups. */
.titlebar-divider {
  flex: none;
  width: 1px;
  height: 16px;
  margin: 0 6px;
  background: var(--app-border);
}

/* Element Plus teleports some overlays to body. Read the live window geometry
   there too, so every dialog/message box respects chrome and maximization. */
body:has(.window-frame) > .el-overlay,
.window-frame .el-overlay {
  --overlay-inset: var(--window-overlay-inset, 1px);
  --overlay-radius: var(--window-overlay-radius, var(--window-inner-radius));
  top: calc(var(--titlebar-height) + var(--overlay-inset));
  right: var(--overlay-inset);
  bottom: var(--overlay-inset);
  left: var(--overlay-inset);
  height: auto;
  border-radius: 0 0 var(--overlay-radius) var(--overlay-radius);
  overflow: hidden;
  clip-path: inset(0 round 0 0 var(--overlay-radius) var(--overlay-radius));
}

body:has(.window-frame.is-maximized) > .el-overlay {
  --window-overlay-inset: 0px;
  --window-overlay-radius: 0px;
}

body:has(.window-frame) .el-overlay-dialog {
  position: absolute;
  inset: 0;
}

:root {
  /* Window chrome geometry: the single source of truth for the rounded
     transparent frame. The titlebar carries the inner radius so its
     backdrop-filter layer is clipped by its own corners instead of relying
     on the parent's overflow, which WebKit does not apply to composited
     backdrop-filter layers. */
  --titlebar-height: 42px;
  --window-radius: 15px;
  --window-inner-radius: calc(var(--window-radius) - 1px);

  /* Reference accents: one perceptual base per hue; variants derive from it. */
  --accent-blue: oklch(58% 0.19 262);
  --accent-red: oklch(56% 0.2 25);
  --accent-purple: oklch(54% 0.21 302);
  --accent-green: oklch(60% 0.15 152);
  --accent-yellow: oklch(72% 0.16 92);

  --accent: var(--accent-blue);
  --accent-rgb: 54, 114, 233;
  --selection-bg: linear-gradient(rgba(var(--accent-rgb), 0.14), rgba(var(--accent-rgb), 0.14)) var(--app-surface);
  --selection-border: rgba(var(--accent-rgb), 0.5);
  --accent-on: #ffffff;

  /* 语义色：一个颜色只表示一个含义。强调色只用于可操作元素（主按钮、当前选中、
     链接），状态另用一组低饱和色相：绿=已就绪，红=失败/离线，琥珀=等你处理，
     灰蓝=中性事实（数量、未申请、已取消）。色阶由下面两个混合基色推导，
     明暗外观各换一套基色，因此状态色在两种外观下都不会跑偏。 */
  --tone-success: oklch(45% 0.12 152);
  --tone-danger: oklch(46% 0.17 27);
  --tone-warning: oklch(45% 0.11 70);
  --tone-info: var(--app-text-secondary);
  --tone-mix-base: white;
  --tone-mix-shade: black;

  /* Semantic tokens (light theme). */
  --app-text: #16202e;
  --app-text-strong: #334155;
  --app-text-secondary: #5a6879;
  --app-text-muted: #5f6e83;
  --app-surface: #ffffff;
  --app-surface-2: #f7f9fc;
  --app-surface-3: #fbfcfe;
  --app-bg-solid: #f3f6fa;
  --app-border: #e3e9f0;
  --app-frame-border: rgba(22, 32, 46, 0.08);
  --titlebar-bg: rgba(255, 255, 255, 0.6);
  --titlebar-border: rgba(227, 233, 240, 0.9);
  --pill-bg: #eef2f7;
  --pill-fg: #526072;
  --code-bg: #fffaf0;
  --code-border: #f0c774;
  --code-text: #3a2a10;
  --syntax-comment: #7a8699;
  --syntax-keyword: #a626a4;
  --syntax-string: #2f7d32;
  --syntax-number: #9a6700;
  --syntax-title: #3b6fd4;
  --syntax-built-in: #9a6700;
  --syntax-variable: #b54708;
  --syntax-deletion: #c0392b;

  --el-border-radius-base: 10px;
  --el-component-size: 38px;
  font-family:
    Inter, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", Avenir,
    Helvetica, Arial, sans-serif;
  color: var(--app-text);
  background: transparent;
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

/* Element Plus primary palette derived from the active accent. The extra
   attribute gives this block higher specificity than Element Plus's own
   `:root` and `html.dark` defaults, so light and dark themes both follow the
   selected accent regardless of stylesheet order. */
:root[data-theme] {
  --el-color-primary: var(--accent);
  --el-color-primary-light-3: color-mix(in oklab, var(--accent) 70%, white);
  --el-color-primary-light-5: color-mix(in oklab, var(--accent) 50%, white);
  --el-color-primary-light-7: color-mix(in oklab, var(--accent) 30%, white);
  --el-color-primary-light-8: color-mix(in oklab, var(--accent) 20%, white);
  --el-color-primary-light-9: color-mix(in oklab, var(--accent) 10%, white);
  --el-color-primary-dark-2: color-mix(in oklab, var(--accent) 80%, black);

  /* Element Plus 的语义色阶同样从 toned 基色推导：plain 标签的底色取 light-9、
     描边取 light-8，文字取基色，因此状态标签只有一层真正着色（文字），
     底色淡到几乎与面板同色，不会和强调色按钮抢视线。 */
  --el-color-success: var(--tone-success);
  --el-color-success-light-3: color-mix(in oklab, var(--tone-success) 80%, var(--tone-mix-base));
  --el-color-success-light-5: color-mix(in oklab, var(--tone-success) 65%, var(--tone-mix-base));
  --el-color-success-light-7: color-mix(in oklab, var(--tone-success) 45%, var(--tone-mix-base));
  --el-color-success-light-8: color-mix(in oklab, var(--tone-success) 22%, var(--tone-mix-base));
  --el-color-success-light-9: color-mix(in oklab, var(--tone-success) 10%, var(--tone-mix-base));
  --el-color-success-dark-2: color-mix(in oklab, var(--tone-success) 85%, var(--tone-mix-shade));

  --el-color-danger: var(--tone-danger);
  --el-color-danger-light-3: color-mix(in oklab, var(--tone-danger) 80%, var(--tone-mix-base));
  --el-color-danger-light-5: color-mix(in oklab, var(--tone-danger) 65%, var(--tone-mix-base));
  --el-color-danger-light-7: color-mix(in oklab, var(--tone-danger) 45%, var(--tone-mix-base));
  --el-color-danger-light-8: color-mix(in oklab, var(--tone-danger) 22%, var(--tone-mix-base));
  --el-color-danger-light-9: color-mix(in oklab, var(--tone-danger) 10%, var(--tone-mix-base));
  --el-color-danger-dark-2: color-mix(in oklab, var(--tone-danger) 85%, var(--tone-mix-shade));

  /* ElMessage uses "error", while buttons and tags use "danger". */
  --el-color-error: var(--el-color-danger);
  --el-color-error-light-3: var(--el-color-danger-light-3);
  --el-color-error-light-5: var(--el-color-danger-light-5);
  --el-color-error-light-7: var(--el-color-danger-light-7);
  --el-color-error-light-8: var(--el-color-danger-light-8);
  --el-color-error-light-9: var(--el-color-danger-light-9);
  --el-color-error-dark-2: var(--el-color-danger-dark-2);

  --el-color-warning: var(--tone-warning);
  --el-color-warning-light-3: color-mix(in oklab, var(--tone-warning) 80%, var(--tone-mix-base));
  --el-color-warning-light-5: color-mix(in oklab, var(--tone-warning) 65%, var(--tone-mix-base));
  --el-color-warning-light-7: color-mix(in oklab, var(--tone-warning) 45%, var(--tone-mix-base));
  --el-color-warning-light-8: color-mix(in oklab, var(--tone-warning) 22%, var(--tone-mix-base));
  --el-color-warning-light-9: color-mix(in oklab, var(--tone-warning) 10%, var(--tone-mix-base));
  --el-color-warning-dark-2: color-mix(in oklab, var(--tone-warning) 85%, var(--tone-mix-shade));

  --el-color-info: var(--tone-info);
  --el-color-info-light-3: color-mix(in oklab, var(--tone-info) 80%, var(--tone-mix-base));
  --el-color-info-light-5: color-mix(in oklab, var(--tone-info) 65%, var(--tone-mix-base));
  --el-color-info-light-7: color-mix(in oklab, var(--tone-info) 45%, var(--tone-mix-base));
  --el-color-info-light-8: color-mix(in oklab, var(--tone-info) 22%, var(--tone-mix-base));
  --el-color-info-light-9: color-mix(in oklab, var(--tone-info) 10%, var(--tone-mix-base));
  --el-color-info-dark-2: color-mix(in oklab, var(--tone-info) 85%, var(--tone-mix-shade));
}

/* Unsupported functions inside custom properties do not fall back to earlier
   declarations. Override the consuming properties with ordinary theme colors. */
@supports not (color: color-mix(in oklab, red, white)) {
  :root .el-message {
    background-color: var(--app-surface);
    border-color: var(--app-border);
  }

  :root .el-message .el-message__content,
  :root .el-message [class*="el-message-icon--"] {
    color: var(--app-text);
  }
}

/* Accent switching: only the base changes; all tints follow. */
:root[data-accent="red"] {
  --accent: var(--accent-red);
  --accent-rgb: 200, 60, 62;
}
:root[data-accent="purple"] {
  --accent: var(--accent-purple);
  --accent-rgb: 143, 65, 200;
}
:root[data-accent="green"] {
  --accent: var(--accent-green);
  --accent-rgb: 38, 151, 89;
}
:root[data-accent="yellow"] {
  --accent: var(--accent-yellow);
  --accent-rgb: 193, 161, 0;
  /* A bright yellow needs dark text to stay readable on filled controls. */
  --accent-on: #241a00;
}

/* Solid primary buttons use the accent's on-colour. Plain, text and link
   variants keep Element Plus's accent-coloured foreground so they stay
   readable on their near-white surfaces. */
:root .el-button--primary:not(.is-plain):not(.is-text):not(.is-link):not(.list-pill) {
  --el-button-text-color: var(--accent-on);
  --el-button-hover-text-color: var(--accent-on);
  --el-button-active-text-color: var(--accent-on);
}

/* Element Plus sets a 32px button size and input wrapper height on the
   component roots, so the shared component-size token alone leaves buttons
   shorter than text fields. Pin both to the same control height. */
:root .el-button {
  --el-button-size: var(--el-component-size);
  height: var(--el-component-size);
}

:root .el-input__wrapper,
:root .el-select__wrapper {
  height: var(--el-component-size);
  min-height: var(--el-component-size);
  box-sizing: border-box;
}

/* Dark theme overrides. [data-theme=dark] (specificity 0,2,0) beats Element
   Plus's html.dark block (0,1,1), so the accent-derived palette survives. */
:root[data-theme="dark"] {
  --app-text: #e6ebf2;
  --app-text-strong: #c4cddb;
  --app-text-secondary: #a7b2c0;
  --app-text-muted: #8b98a9;
  --app-surface: #1c2128;
  --app-surface-2: #232a33;
  --app-surface-3: #20262e;
  --app-bg-solid: #14181f;
  --app-border: #30363d;
  --app-frame-border: rgba(0, 0, 0, 0.5);
  --titlebar-bg: rgba(28, 33, 40, 0.6);
  --titlebar-border: rgba(48, 54, 61, 0.9);
  --pill-bg: #2a313b;
  --pill-fg: #aeb9c6;
  --code-bg: #2a2410;
  --code-border: #6b5a2a;
  --code-text: #f0dca0;
  --syntax-comment: #8b98a9;
  --syntax-keyword: #ff7b72;
  --syntax-string: #a5d6ff;
  --syntax-number: #79c0ff;
  --syntax-title: #d2a8ff;
  --syntax-built-in: #ffa657;
  --syntax-variable: #ffa657;
  --syntax-deletion: #ff7b72;

  /* In dark mode the lighter steps darken toward the surface. */
  --el-color-primary-light-3: color-mix(in oklab, var(--accent) 80%, black);
  --el-color-primary-light-5: color-mix(in oklab, var(--accent) 65%, black);
  --el-color-primary-light-7: color-mix(in oklab, var(--accent) 50%, black);
  --el-color-primary-light-8: color-mix(in oklab, var(--accent) 40%, black);
  --el-color-primary-light-9: color-mix(in oklab, var(--accent) 25%, black);
  --el-color-primary-dark-2: color-mix(in oklab, var(--accent) 80%, white);

  /* 暗色外观下的状态色：底往面板色压深、字提亮，不做简单反相。
     与浅色共用同一套推导规则，只换基色，所以两种外观的层次一致。 */
  --tone-success: oklch(80% 0.14 152);
  --tone-danger: oklch(80% 0.13 25);
  --tone-warning: oklch(84% 0.13 85);
  --tone-mix-base: var(--app-surface-2);
  --tone-mix-shade: white;
}

html,
body,
#app {
  height: 100%;
  margin: 0;
  background: transparent;
  overflow: hidden;
}

/* Shared layout primitives used by the auth, settings and peer views. */
.card {
  background: var(--app-surface);
  border: 1px solid var(--app-border);
  border-radius: 16px;
  padding: 24px;
  box-shadow: 0 14px 40px rgba(22, 32, 46, 0.06);
}

.card h2 {
  margin: 0 0 12px;
  font-size: 19px;
  color: var(--app-text);
}

.card p {
  margin: 0 0 18px;
  color: var(--app-text-secondary);
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

/* 列表设计语言的全局件：标题数量角标、行内标签、行内动作按钮在首页/设置/文件页共用
   一套几何，写在组件里会各写一份然后慢慢跑偏。 */
.count-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 20px;
  height: 20px;
  padding: 0 6px;
  margin-left: 8px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
  vertical-align: 2px;
  /* 数量是中性事实，用灰色层而不是强调色：强调色只代表「可操作」。 */
  color: var(--pill-fg);
  background: var(--pill-bg);
  font-variant-numeric: tabular-nums;
}

/* 行内标签不参与伸缩，保证多栏列表的列对齐。 */
.row-tag {
  flex: none;
}

/* 状态标签只靠底色与文字表达，描边去掉：底、边、字三层同时着色是「颜色杂」
   的主要来源。12px 小字改用中等字重，避免细体在浅底上发虚。 */
.el-tag.row-tag {
  border-color: transparent;
  font-weight: 500;
}

/* 列表行里的动作按钮比表单控件矮一档。上面的 :root .el-button 把高度钉在控件高度，
   这里用 :root 前缀抬高权重覆盖，只影响显式带 list-pill 的按钮，不动表单按钮。 */
:root .el-button.list-pill {
  --el-button-size: 24px;
  height: 24px;
  padding: 0 10px;
  margin: 0;
  /* 行内动作一律降级为中性描边：一屏里只允许面板级主按钮用强调色底，
     否则每行两颗实心按钮会把状态标签挤成噪音，也让人分不清先点哪个。 */
  --el-button-bg-color: var(--app-surface);
  --el-button-border-color: var(--app-border);
  --el-button-text-color: var(--app-text-strong);
  --el-button-hover-bg-color: var(--app-surface-2);
  --el-button-hover-border-color: color-mix(in oklab, var(--accent) 45%, var(--app-border));
  --el-button-hover-text-color: var(--accent);
  --el-button-active-bg-color: var(--pill-bg);
  --el-button-active-border-color: color-mix(in oklab, var(--accent) 60%, var(--app-border));
  --el-button-active-text-color: var(--accent);
  /* 禁用态也必须保持中性面：EP 默认给主类型算出的是淡强调色底，
     请求中会闪出一块品牌色，正好破坏「强调色只表示可点」。 */
  --el-button-disabled-bg-color: var(--app-surface);
  --el-button-disabled-border-color: var(--app-border);
  --el-button-disabled-text-color: var(--app-text-muted);
}

/* 面板级主操作（如「添加共享」）保留强调色底，作为该视图唯一的突出按钮。 */
:root .el-button.list-pill.pane-cta {
  --el-button-bg-color: var(--accent);
  --el-button-border-color: var(--accent);
  --el-button-text-color: var(--accent-on);
  --el-button-hover-bg-color: var(--el-color-primary-light-3);
  --el-button-hover-border-color: var(--el-color-primary-light-3);
  --el-button-hover-text-color: var(--accent-on);
  --el-button-active-bg-color: var(--el-color-primary-dark-2);
  --el-button-active-border-color: var(--el-color-primary-dark-2);
  --el-button-active-text-color: var(--accent-on);
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
  color: var(--app-text-strong);
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
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
}

.server-panel {
  margin-top: 26px;
  padding-top: 22px;
  border-top: 1px solid var(--app-border);
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.server-panel:first-child {
  margin-top: 0;
  padding-top: 0;
  border-top: 0;
}

.server-panel h3 {
  margin: 0;
  font-size: 16px;
  color: var(--app-text);
}

.hint {
  margin: 0;
  color: var(--app-text-secondary);
  font-size: 13px;
  line-height: 1.5;
}

.quit-body {
  margin: 0;
  color: var(--app-text-secondary);
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

.grid .el-input-number,
.grid .el-select {
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
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
}

.code-box {
  padding: 14px;
  border-radius: 12px;
  background: var(--code-bg);
  border: 1px solid var(--code-border);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.code {
  font-size: 14px;
  word-break: break-all;
  color: var(--code-text);
}

.footer {
  text-align: center;
  font-size: 12px;
  color: var(--app-text-muted);
  padding-top: 8px;
}

/* Dark-mode tints for inline warning text. */
:root[data-theme="dark"] .warning {
  color: #e0a94f !important;
}
</style>
