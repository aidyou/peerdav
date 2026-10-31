<script setup>
import { h, onMounted, ref } from "vue";
import { cloud, blankConnection } from "../lib/cloud";
import { connectionEditor, parseS3Url } from "../lib/s3Url";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t } from "../lib/i18n";
import { writeRoute } from "../lib/navigation";

const app = useAppContext();
const data = ref({ connections: [], mappings: [] });
const connection = ref(blankConnection());
const editorOpen = ref(false);
const advancedOpen = ref(false);
const url = ref("");
const needsDetails = ref(false);
const loading = ref(false);
function fail(error) { app.handleFailure(error); }
function notifyConnectionsChanged() {
  if (typeof globalThis.dispatchEvent === "function" && typeof globalThis.CustomEvent === "function") globalThis.dispatchEvent(new CustomEvent("peerdav:cloud-changed"));
}
async function refresh() { try { data.value = await cloud.list(); } catch (error) { fail(error); } }
function selectConnection(item) {
  connection.value = connectionEditor(item);
  url.value = item?.endpoint || "";
  needsDetails.value = false; advancedOpen.value = false; editorOpen.value = true;
}
function changeUrl(value) {
  url.value = value;
  const result = parseS3Url(value);
  connection.value.endpoint = result.endpoint;
  connection.value.bucket = result.bucket;
  connection.value.region = result.region;
  // object_store requires bucket-bearing endpoints for virtual-hosted mode.
  // Our normalized service endpoints therefore use path-style requests.
  if (result.known) connection.value.pathStyle = true;
  needsDetails.value = !result.bucket || !result.region;
  if (needsDetails.value) advancedOpen.value = true;
}
async function saveConnection() {
  if (!connection.value.bucket || !connection.value.region) { needsDetails.value = true; advancedOpen.value = true; app.setError("drive.detailsRequired"); return; }
  loading.value = true;
  try {
    await cloud.saveConnection({ ...connection.value, accessKey: connection.value.accessKey || undefined, secretKey: connection.value.secretKey || undefined, sessionToken: connection.value.sessionToken || undefined });
    notifyConnectionsChanged();
    connection.value = blankConnection(); editorOpen.value = false; url.value = "";
    await refresh(); app.setNotice("cloud.saved");
  } catch (error) { fail(error); } finally { loading.value = false; }
}
async function testConnection(item) { if (!item?.id) return; try { await cloud.testConnection(String(item.id)); await refresh(); } catch (error) { fail(error); } }
async function deleteConnection(item) {
  if (!item?.id) return;
  const count = (data.value.mappings || []).filter((m) => m.connectionId === item.id).length;
  const message = h("div", [h("p", t("drive.removeConfirm", { name: item.name })), ...(count ? [h("p", { style: { color: "var(--el-color-danger, #d03050)", fontWeight: "bold" }, "data-test": "sync-removal-warning" }, t("drive.removeWarning", { count }))] : [])]);
  if (!await confirmAction(message, t("drive.remove"), { type: count ? "error" : "warning" })) return;
  try { await cloud.deleteConnection(String(item.id)); notifyConnectionsChanged(); if (connection.value.id === item.id) { connection.value = blankConnection(); editorOpen.value = false; } await refresh(); } catch (error) { fail(error); }
}
onMounted(refresh);
</script>
<template>
  <div class="cloud-settings">
    <h3>{{ t('drive.connections') }}</h3><p>{{ t('drive.settingsHint') }}</p>
    <el-button class="connection-add" @click="selectConnection()">{{ t('drive.add') }}</el-button>
    <div v-for="item in data.connections" :key="item.id" class="cloud-item"><strong>{{ item.name }}</strong><span>{{ item.endpoint }} / {{ item.bucket }}</span><el-button class="cloud-browse-entry" @click="writeRoute({ view: 'cloud', connectionId: String(item.id) })">{{ t('cloudBrowser.open') }}</el-button><el-button class="connection-edit" @click="selectConnection(item)">{{ t('cloud.edit') }}</el-button><el-button class="connection-remove" @click="deleteConnection(item)">{{ t('drive.remove') }}</el-button></div>
    <div v-if="editorOpen" class="settings-group connection-editor"><h3 class="group-title">{{ t('drive.connectionDetails') }}</h3>
      <div class="settings-rows">
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.name') }}</span><el-input v-model="connection.name" class="connection-name" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('drive.url') }}</span><el-input :model-value="url" class="connection-url" @update:model-value="changeUrl" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.accessKey') }}</span><el-input v-model="connection.accessKey" autocomplete="off" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.secretKey') }}</span><el-input v-model="connection.secretKey" type="password" autocomplete="new-password" /></label>
        <p v-if="needsDetails" role="status">{{ t('drive.detailsRequired') }}</p>
        <details :open="advancedOpen" class="connection-advanced" @toggle="advancedOpen = $event.target.open"><summary>{{ t('drive.advanced') }}</summary>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.bucket") }}</span><el-input v-model="connection.bucket" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.region") }}</span><el-input v-model="connection.region" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.prefix") }}</span><el-input v-model="connection.prefix" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.sessionToken") }}</span><el-input v-model="connection.sessionToken" type="password" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxy") }}</span><el-select v-model="connection.proxy.mode"><el-option value="direct" :label="t('cloud.proxyDirect')" /><el-option value="environment" :label="t('cloud.proxyEnvironment')" /><el-option value="custom" :label="t('cloud.proxyCustom')" /></el-select></label>
        <template v-if="connection.proxy.mode === 'custom'"><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyUrl") }}</span><el-input v-model="connection.proxy.url" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyUsername") }}</span><el-input v-model="connection.proxy.username" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.proxyPassword") }}</span><el-input v-model="connection.proxy.password" type="password" /></label></template>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.allowInsecureHttp") }}</span><el-switch v-model="connection.allowInsecureHttp" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.pathStyle") }}</span><el-switch v-model="connection.pathStyle" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearCredentials") }}</span><el-switch v-model="connection.clearCredentials" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearSessionToken") }}</span><el-switch v-model="connection.clearSessionToken" /></label><label class="settings-row"><span class="settings-row__label">{{ t("cloud.clearProxyAuth") }}</span><el-switch v-model="connection.clearProxyAuth" /></label>
        </details>
        <div class="settings-row settings-row--actions"><el-button class="connection-save" type="primary" :loading="loading" @click="saveConnection">{{ t('cloud.save') }}</el-button><el-button v-if="connection.id" @click="testConnection(connection)">{{ t('cloud.test') }}</el-button><el-button @click="editorOpen = false; connection = blankConnection()">{{ t('confirm.cancel') }}</el-button></div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.cloud-settings { min-width: 0; }
.cloud-item { display: flex; flex-wrap: wrap; gap: 0.5rem; min-width: 0; overflow-wrap: anywhere; align-items: center; }
.cloud-item > span { min-width: 0; }
.cloud-settings .settings-row { min-width: 0; flex-wrap: wrap; gap: 0.5rem; }
.cloud-settings :deep(.el-input), .cloud-settings :deep(.el-select) { min-width: 0; max-width: 100%; }
.cloud-preview pre { white-space: pre-wrap; overflow-wrap: anywhere; }
.cloud-safety-hint, .cloud-progress, .append-trust { overflow-wrap: anywhere; line-height: 1.6; }
.append-settings, .append-trust { min-width: 0; max-width: 100%; }
@media (max-width: 400px) { .cloud-settings .settings-row > :deep(.el-select), .cloud-settings .settings-row > :deep(.el-input) { width: 100%; flex: 1 1 100%; } }
@media (max-width: 480px) { .cloud-settings .settings-row { align-items: stretch; } .cloud-settings .settings-row__label { flex-basis: 100%; } .cloud-settings .settings-row input { max-width: 100%; min-width: 0; } }
@media (max-width: 400px) { .cloud-item { align-items: flex-start; } }
</style>
