<script setup>
import { computed, ref } from "vue";
import { open } from "@tauri-apps/plugin-dialog";

import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";
import { validatePassword } from "../lib/password";
import { useAction } from "../composables/useAction";

defineProps({
  view: { type: String, required: true },
});

const app = useAppContext();
const { busy, run } = useAction();

const initStep = ref(1);
const keyDirectory = ref("");
const initAdminUsername = ref("");
const initAdminPassword = ref("");
const initAdminPasswordConfirm = ref("");
const showResetForm = ref(false);
const resetRecoveryCode = ref("");
const resetDirectory = ref("");
const resetAdminUsername = ref("");
const resetAdminPassword = ref("");
const resetAdminPasswordConfirm = ref("");
const resetConfirmation = ref("");
const recoveryCode = ref("");
const showRecoveryCode = ref(false);
const loginUsername = ref(app.adminUsername);
const loginPassword = ref("");
const adminCurrentPassword = ref("");
const adminNewPassword = ref("");
const adminNewPasswordConfirm = ref("");

const keyErrorText = computed(() => {
  const code = app.status.keyError;
  if (!code) return "";
  const key = `vault.keyError.${code}`;
  const localized = t(key);
  return localized === key ? t("vault.keyError.key_file_unavailable") : localized;
});

const hasKeyFile = computed(() => Boolean(app.status.keyFile));
const keyUnavailable = computed(
  () => !app.status.keyFile || Boolean(app.status.keyError),
);

function goToAdminStep() {
  app.clearMessages();
  if (!keyDirectory.value.trim()) {
    app.setError("vault.keyDirectoryRequired");
    return;
  }
  initStep.value = 2;
}

function backToEncryptionStep() {
  app.clearMessages();
  initStep.value = 1;
}

async function initializeVault() {
  if (initStep.value === 1) {
    goToAdminStep();
    return;
  }

  app.clearMessages();
  const passwordError = validatePassword(initAdminPassword.value);
  if (passwordError) {
    app.setError(passwordError);
    return;
  }
  if (initAdminPassword.value !== initAdminPasswordConfirm.value) {
    app.setError("vault.adminPasswordMismatch");
    return;
  }

  await run(async () => {
    const result = await app.invoke("initialize_vault", {
      keyDirectory: keyDirectory.value,
      adminUsername: initAdminUsername.value,
      adminPassword: initAdminPassword.value,
    });
    keyDirectory.value = "";
    recoveryCode.value = result.recoveryCode;
    showRecoveryCode.value = true;
    initAdminUsername.value = result.adminUsername;
    initAdminPassword.value = "";
    initAdminPasswordConfirm.value = "";
    initStep.value = 1;
    app.applyVaultStatus(result.vaultStatus);
    app.setAdminUsername(result.adminUsername);
    loginUsername.value = result.adminUsername;
    app.setNotice("vault.initialized");
  });
}

async function chooseDirectory(target) {
  app.clearMessages();
  const selected = await open({
    directory: true,
    multiple: false,
    title: t("vault.chooseDirectory"),
  });
  if (typeof selected !== "string") return;
  if (target === "initialize") {
    keyDirectory.value = selected;
  } else {
    resetDirectory.value = selected;
  }
}

async function reloadVaultKey(path) {
  await run(async () => {
    const next = await app.invoke("reload_vault_key", {
      path: path ?? null,
    });
    app.applyVaultStatus(next);
    await app.resumeServer();
    app.setNotice("vault.keyLoaded");
  });
}

async function chooseKeyFile() {
  app.clearMessages();
  const selected = await open({
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
  await reloadVaultKey(selected);
}

async function copyRecoveryCode() {
  try {
    await navigator.clipboard.writeText(recoveryCode.value);
    app.setNotice("vault.recoveryCopied");
  } catch (error) {
    app.handleFailure(error);
  }
}

async function resetVault() {
  app.clearMessages();
  if (!resetRecoveryCode.value.trim()) {
    app.setError("vault.resetRecoveryRequired");
    return;
  }
  if (!resetDirectory.value.trim()) {
    app.setError("vault.keyDirectoryRequired");
    return;
  }
  if (resetConfirmation.value.trim().toUpperCase() !== "RESET") {
    app.setError("vault.resetConfirmationMismatch");
    return;
  }
  const passwordError = validatePassword(resetAdminPassword.value);
  if (passwordError) {
    app.setError(passwordError);
    return;
  }
  if (resetAdminPassword.value !== resetAdminPasswordConfirm.value) {
    app.setError("vault.adminPasswordMismatch");
    return;
  }

  await run(async () => {
    const result = await app.invoke("reset_vault", {
      recoveryCode: resetRecoveryCode.value,
      keyDirectory: resetDirectory.value,
      adminUsername: resetAdminUsername.value,
      adminPassword: resetAdminPassword.value,
    });
    app.applyVaultStatus(result.vaultStatus);
    app.setAdminUsername(result.adminUsername);
    loginUsername.value = result.adminUsername;
    recoveryCode.value = result.recoveryCode;
    showRecoveryCode.value = true;
    showResetForm.value = false;
    resetRecoveryCode.value = "";
    resetDirectory.value = "";
    resetAdminUsername.value = "";
    resetAdminPassword.value = "";
    resetAdminPasswordConfirm.value = "";
    resetConfirmation.value = "";
    app.setNotice("vault.resetDone", {
      path: result.archivedDatabase,
    });
    await app.refreshServerStatus();
  });
}

async function loginAdmin() {
  const password = loginPassword.value;
  await run(async () => {
    const result = await app.invoke("admin_login", {
      username: loginUsername.value,
      password,
    });
    app.completeLogin(result, password);
    loginPassword.value = "";
    app.setNotice(
      result.mustChangePassword ? "login.mustChange" : "login.succeeded",
    );
    await app.refreshServerStatus();
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
    await app.refreshServerStatus();
  });
}

function backToLogin() {
  app.resetAdminSession();
  loginPassword.value = "";
  app.clearMessages();
}
</script>

<template>
  <section v-if="view === 'loading'" class="startup-loading" role="status" aria-live="polite" aria-busy="true">
    <p>{{ t("common.loading") }}</p>
  </section>

  <section v-else-if="view === 'initialize'" class="card">
    <h2>{{ t("vault.initializeTitle") }}</h2>
    <p>{{ t("vault.initializeBody") }}</p>
    <form class="stack" @submit.prevent="initializeVault">
      <el-steps :active="initStep - 1" simple>
        <el-step :title="t('vault.stepEncryption')" />
        <el-step :title="t('vault.stepAdmin')" />
      </el-steps>
      <template v-if="initStep === 1">
        <label>
          {{ t("vault.keyDirectory") }}
          <div class="path-picker">
            <el-input
              :model-value="keyDirectory"
              type="text"
              readonly
              :placeholder="t('vault.keyDirectoryPlaceholder')"
            />
            <el-button plain @click="chooseDirectory('initialize')">
              {{ t("vault.chooseDirectory") }}
            </el-button>
          </div>
        </label>
        <p class="hint">{{ t("vault.keyDirectoryHint") }}</p>
        <p class="warning">{{ t("vault.keyBackupWarning") }}</p>
        <div class="actions">
          <el-button type="primary" @click="goToAdminStep">
            {{ t("vault.nextStep") }}
          </el-button>
        </div>
      </template>

      <template v-else>
        <p class="hint">{{ t("vault.passwordRule") }}</p>
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
    <h2>{{ t("vault.recoverTitle") }}</h2>
    <p>{{ t("vault.recoverBody") }}</p>
    <p v-if="keyErrorText" class="warning">{{ keyErrorText }}</p>
    <p v-if="app.status.keyFile" class="hint mono">
      {{ t("vault.keyFilePath") }}: {{ app.status.keyFile }}
    </p>
    <div class="actions">
      <el-button
        v-if="!keyUnavailable"
        type="primary"
        :disabled="busy"
        @click="reloadVaultKey()"
      >
        {{ t("vault.reloadKey") }}
      </el-button>
      <el-button
        v-else
        type="primary"
        :disabled="busy"
        @click="showResetForm = true"
      >
        {{ t("vault.generateKey") }}
      </el-button>
      <el-button
        v-if="keyUnavailable && hasKeyFile"
        plain
        :disabled="busy"
        @click="reloadVaultKey()"
      >
        {{ t("vault.reloadKey") }}
      </el-button>
      <el-button plain :disabled="busy" @click="chooseKeyFile">
        {{ t("vault.chooseKeyFile") }}
      </el-button>
      <el-button plain :disabled="busy" @click="showResetForm = !showResetForm">
        {{ t("vault.resetShow") }}
      </el-button>
    </div>
    <p v-if="keyUnavailable" class="warning">
      {{ t("vault.generateKeyHint") }}
    </p>
    <p class="hint">{{ t("vault.keyLostHint") }}</p>

    <form
      v-if="showResetForm"
      class="stack inset-form"
      @submit.prevent="resetVault"
    >
      <p class="warning">{{ t("vault.resetWarning") }}</p>
      <label>
        {{ t("vault.resetRecoveryCode") }}
        <el-input
          v-model="resetRecoveryCode"
          type="password"
          show-password
          autocomplete="off"
          required
        />
      </label>
      <label>
        {{ t("vault.resetNewDirectory") }}
        <div class="path-picker">
          <el-input
            :model-value="resetDirectory"
            type="text"
            readonly
            :placeholder="t('vault.keyDirectoryPlaceholder')"
          />
          <el-button plain @click="chooseDirectory('reset')">
            {{ t("vault.chooseDirectory") }}
          </el-button>
        </div>
      </label>
      <label>
        {{ t("vault.adminUsername") }}
        <el-input
          v-model="resetAdminUsername"
          type="text"
          autocomplete="username"
          maxlength="64"
          required
        />
      </label>
      <label>
        {{ t("vault.adminPassword") }}
        <el-input
          v-model="resetAdminPassword"
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
          v-model="resetAdminPasswordConfirm"
          type="password"
          show-password
          minlength="6"
          autocomplete="new-password"
          required
        />
      </label>
      <label>
        {{ t("vault.resetConfirmation") }}
        <el-input
          v-model="resetConfirmation"
          type="text"
          placeholder="RESET"
          autocomplete="off"
          required
        />
      </label>
      <el-button type="danger" native-type="submit" :disabled="busy">
        {{ keyUnavailable ? t("vault.generateSubmit") : t("vault.resetSubmit") }}
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
      <el-button type="primary" native-type="submit" :disabled="busy" :loading="busy">
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
        <el-button plain @click="backToLogin">
          {{ t("login.back") }}
        </el-button>
      </div>
    </form>
  </section>

  <el-dialog
    v-model="showRecoveryCode"
    :title="t('vault.recoveryTitle')"
    width="520px"
    :close-on-click-modal="false"
    :close-on-press-escape="false"
    :show-close="false"
  >
    <p>{{ t("vault.recoveryBody") }}</p>
    <p class="mono recovery-code">{{ recoveryCode }}</p>
    <p class="warning">{{ t("vault.recoveryWarning") }}</p>
    <template #footer>
      <el-button plain @click="copyRecoveryCode">
        {{ t("vault.recoveryCopy") }}
      </el-button>
      <el-button type="primary" @click="showRecoveryCode = false">
        {{ t("vault.recoverySaved") }}
      </el-button>
    </template>
  </el-dialog>
</template>

<style scoped>
/* Keep the window background stable; fast restores show no interim card. */
.startup-loading {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--app-text-muted);
  font-size: 13px;
}

.startup-loading p {
  margin: 0;
}

.path-picker {
  display: flex;
  align-items: center;
  gap: 8px;
}

.path-picker :deep(.el-input) {
  flex: 1;
  min-width: 0;
}

.recovery-code {
  padding: 12px;
  border: 1px solid var(--app-border);
  border-radius: 8px;
  background: var(--app-surface-3);
  font-size: 18px;
  line-height: 1.5;
  letter-spacing: 0.04em;
  overflow-wrap: anywhere;
  user-select: all;
}
</style>