<script setup>
import { computed, onMounted, onUnmounted, provide, reactive, ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ElMessage } from "element-plus";

import { errorStrategy, invoke, normalizeAppError } from "./lib/invoke";
import { locale, localizeMessage, setLocale, t } from "./lib/i18n";
import { resolvedTheme, setTheme, theme } from "./lib/theme";
import { SUPPORTED_ACCENTS, accent, setAccent } from "./lib/accent";
import {
  markSettingsSeen,
  shouldOpenSettingsOnLogin,
} from "./lib/workspace";
import { APP_CONTEXT } from "./lib/appContext";
import AppRail from "./components/AppRail.vue";
import AuthView from "./components/AuthView.vue";
import HomeView from "./components/HomeView.vue";
import PeerBrowser from "./components/PeerBrowser.vue";
import SettingsView from "./components/SettingsView.vue";
import TitleBarMenu from "./components/TitleBarMenu.vue";

const loading = ref(true);
const status = ref({
  initialized: false,
  unlocked: false,
  keyFile: null,
  keyError: null,
});
const errorState = ref(null);
const showQuitDialog = ref(false);
const isMacOS = /Macintosh|Mac OS X/.test(navigator.userAgent);

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
const peers = ref([]);
const activePeerId = ref("");
const workspaceView = ref("home");
const settingsTab = ref("service");

let eventUnlisten = [];
let activeNotice = null;
let statusRequestInFlight = false;

const errorMessage = computed(() => messageFromState(errorState.value));

function messageFromState(state) {
  if (!state) return "";
  if (state.key) return t(state.key, state.params);
  return localizeMessage(state.raw ?? "");
}

function setError(key, params) {
  errorState.value = { key, params };
}

function setRawError(error) {
  errorState.value = { raw: normalizeAppError(error).message };
}

function setNotice(key, params) {
  activeNotice?.close();
  activeNotice = ElMessage({
    message: t(key, params),
    type: "success",
    duration: 3000,
    showClose: true,
  });
}

function clearMessages() {
  errorState.value = null;
  activeNotice?.close();
  activeNotice = null;
}

const view = computed(() => {
  if (loading.value) return "loading";
  if (!status.value.initialized) return "initialize";
  if (!status.value.unlocked) return "unlock";
  if (mustChangeAdminPassword.value) return "change-admin";
  if (!loggedIn.value) return "login";
  return "ready";
});

const activePeer = computed(
  () => peers.value.find((peer) => peer.id === activePeerId.value) ?? null,
);

onMounted(async () => {
  await bindServerEvents();
  await refreshStatus();
  await restoreAdminSession();
  await refreshServerStatus();
  await resumeServer();
  await refreshPeers();
});

onUnmounted(() => {
  for (const unlisten of eventUnlisten) {
    unlisten();
  }
  eventUnlisten = [];
  activeNotice?.close();
  activeNotice = null;
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
        setNotice("pending.newRequest", { ip: request.peerIp });
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

/// Restores the in-memory admin session after a webview reload. The backend
/// keeps the session until the vault is locked or the admin logs out.
async function restoreAdminSession() {
  if (!status.value.unlocked) return;
  try {
    const session = await invoke("admin_session");
    if (session?.username) {
      adminUsername.value = session.username;
      mustChangeAdminPassword.value = Boolean(session.mustChangePassword);
      loggedIn.value = !session.mustChangePassword;
      if (loggedIn.value) {
        workspaceView.value = "home";
      }
    }
  } catch (error) {
    console.warn("PeerDav admin session restore failed", error);
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
    activePeerId.value = "";
    workspaceView.value = "home";
    void refreshPeers();
  }
}

function landAfterLogin() {
  activePeerId.value = "";
  if (shouldOpenSettingsOnLogin()) {
    markSettingsSeen();
    settingsTab.value = "service";
    workspaceView.value = "settings";
  } else {
    workspaceView.value = "home";
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
  workspaceView.value = "home";
}

async function refreshServerStatus() {
  try {
    serverStatus.value = await invoke("server_status");
  } catch (error) {
    handleFailure(error);
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

async function refreshPeers() {
  try {
    peers.value = await invoke("list_peers");
  } catch (error) {
    const normalized = normalizeAppError(error);
    console.error(`[peerdav:${normalized.code}] saved peers refresh failed`, {
      detail: normalized.detail,
      context: normalized.context,
    });
  }
}

function openPeer(peer) {
  activePeerId.value = peer.id;
  workspaceView.value = "peer";
}

function closePeer() {
  activePeerId.value = "";
  workspaceView.value = "home";
}

function openHome() {
  activePeerId.value = "";
  workspaceView.value = "home";
}

function openSettings() {
  activePeerId.value = "";
  workspaceView.value = "settings";
  settingsTab.value = "service";
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

async function lockVault() {
  try {
    const next = await invoke("lock_vault");
    applyVaultStatus(next);
    setNotice("vault.locked");
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
    peers,
    activePeerId,
    workspaceView,
    settingsTab,
    invoke,
    clearMessages,
    handleFailure,
    setError,
    setRawError,
    setNotice,
    refreshStatus,
    refreshServerStatus,
    refreshPeers,
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
    logout,
    lockVault,
  }),
);
</script>

<template>
  <div class="window-frame">
    <header
      class="titlebar"
      :class="{ 'titlebar--win': !isMacOS }"
      data-tauri-drag-region
    >
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
      <span class="titlebar-title">PeerDav</span>
      <div class="titlebar-actions">
        <TitleBarMenu
          name="language"
          :aria-label="t('titlebar.language')"
          :model-value="locale"
          :items="languageItems"
          @update:model-value="setLocale"
        >
          <template #icon>
            <span class="titlebar-lang">{{
              locale === "zh-CN" ? "中" : "EN"
            }}</span>
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
    </header>

    <main class="shell" :class="{ 'shell--workspace': view === 'ready' }">
      <el-alert
        v-if="errorMessage"
        class="alert alert-error"
        type="error"
        :title="errorMessage"
        :closable="false"
        show-icon
      />

      <AuthView v-if="view !== 'ready'" :view="view" />

      <div v-else class="workspace">
        <AppRail
          :peers="peers"
          :active-peer-id="activePeerId"
          :active-view="workspaceView"
          @open-home="openHome"
          @open-peer="openPeer"
          @open-settings="openSettings"
        />
        <div class="workspace-content">
          <HomeView v-if="workspaceView === 'home'" />
          <PeerBrowser
            v-else-if="workspaceView === 'peer' && activePeer"
            :key="activePeer.id"
            :peer="activePeer"
            @close="closePeer"
          />
          <SettingsView v-else-if="workspaceView === 'settings'" />
          <HomeView v-else />
        </div>
      </div>

      <footer v-if="view !== 'ready'" class="footer">
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
.window-frame {
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-radius: 15px;
  border: 1px solid var(--app-frame-border);
  background:
    radial-gradient(
      circle at top left,
      color-mix(in oklab, var(--accent) 12%, transparent),
      transparent 34%
    ),
    radial-gradient(
      circle at bottom right,
      color-mix(in oklab, var(--accent-green) 10%, transparent),
      transparent 32%
    ),
    var(--app-bg-solid);
  box-shadow: 0 18px 48px rgba(22, 32, 46, 0.18);
}

.titlebar {
  position: relative;
  z-index: 20;
  flex: none;
  height: 42px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  padding: 0 14px;
  background: var(--titlebar-bg);
  border-bottom: 1px solid var(--titlebar-border);
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
  color: var(--app-text-muted);
  letter-spacing: 0.04em;
}

.titlebar-lang {
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
  letter-spacing: 0.02em;
}

.titlebar-actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 2px;
  margin-left: auto;
  -webkit-app-region: no-drag;
}

/* Windows / Linux: window buttons sit right, app controls sit left. */
.titlebar--win .titlebar-actions {
  margin-left: 0;
  order: -1;
}

/* With controls on the left, menus must open toward the content edge so they
   never run off the left side of the window. */
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
  padding: 24px max(20px, calc((100% - 820px) / 2)) 24px;
  box-sizing: border-box;
}

.shell--workspace > .alert {
  margin: 10px 16px 0;
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
:root {
  /* Reference accents: one perceptual base per hue; variants derive from it. */
  --accent-blue: oklch(58% 0.19 262);
  --accent-red: oklch(56% 0.2 25);
  --accent-purple: oklch(54% 0.21 302);
  --accent-green: oklch(60% 0.15 152);
  --accent-yellow: oklch(72% 0.16 92);

  --accent: var(--accent-blue);
  --accent-on: #ffffff;

  /* Semantic tokens (light theme). */
  --app-text: #16202e;
  --app-text-strong: #334155;
  --app-text-secondary: #5a6879;
  --app-text-muted: #6b7a90;
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
}

/* Accent switching: only the base changes; all tints follow. */
:root[data-accent="red"] {
  --accent: var(--accent-red);
}
:root[data-accent="purple"] {
  --accent: var(--accent-purple);
}
:root[data-accent="green"] {
  --accent: var(--accent-green);
}
:root[data-accent="yellow"] {
  --accent: var(--accent-yellow);
  /* A bright yellow needs dark text to stay readable on filled controls. */
  --accent-on: #241a00;
}

/* Solid primary buttons use the accent's on-colour. Scoped under :root so it
   outranks Element Plus's own primary-button defaults regardless of load order. */
:root .el-button--primary {
  --el-button-text-color: var(--accent-on);
  --el-button-hover-text-color: var(--accent-on);
  --el-button-active-text-color: var(--accent-on);
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

  /* In dark mode the lighter steps darken toward the surface. */
  --el-color-primary-light-3: color-mix(in oklab, var(--accent) 80%, black);
  --el-color-primary-light-5: color-mix(in oklab, var(--accent) 65%, black);
  --el-color-primary-light-7: color-mix(in oklab, var(--accent) 50%, black);
  --el-color-primary-light-8: color-mix(in oklab, var(--accent) 40%, black);
  --el-color-primary-light-9: color-mix(in oklab, var(--accent) 25%, black);
  --el-color-primary-dark-2: color-mix(in oklab, var(--accent) 80%, white);
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

.preview-image {
  max-width: 100%;
  max-height: 360px;
  border-radius: 8px;
  border: 1px solid var(--app-border);
  background: var(--app-surface);
  object-fit: contain;
}

.preview-text {
  margin: 0;
  max-height: 320px;
  overflow: auto;
  padding: 12px;
  border-radius: 8px;
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
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