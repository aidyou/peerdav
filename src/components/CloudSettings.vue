<script setup>
import { computed, h, onMounted, ref } from "vue";
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
const loading = ref(false);
const probing = ref(false);
const buckets = ref([]);
const provider = ref("");
// Address-driven discovery: users pick a provider only to prefill the service
// address, then "test" proves the address can list buckets before saving.
const providers = [
  { value: "aws", label: "Amazon S3", endpoint: "https://s3.us-east-1.amazonaws.com", hint: "drive.providerAwsHint" },
  { value: "aliyun", label: "阿里云 OSS", endpoint: "https://oss-cn-hangzhou.aliyuncs.com", hint: "drive.providerAliyunHint" },
  { value: "tencent", label: "腾讯云 COS", endpoint: "https://service.cos.myqcloud.com", hint: "drive.providerTencentHint" },
  { value: "r2", label: "Cloudflare R2", endpoint: "https://<account-id>.r2.cloudflarestorage.com", hint: "drive.providerR2Hint" },
  { value: "qiniu", label: "七牛云", endpoint: "https://s3.cn-east-1.qiniucs.com", hint: "drive.providerQiniuHint" },
  { value: "baidu", label: "百度云 BOS", endpoint: "https://s3.bj.bcebos.com", hint: "drive.providerBaiduHint" },
  { value: "huawei", label: "华为云 OBS", endpoint: "https://obs.cn-north-4.myhuaweicloud.com", hint: "drive.providerHuaweiHint" },
  { value: "gcs", label: "Google Cloud Storage", endpoint: "https://storage.googleapis.com", hint: "drive.providerGcsHint" },
  { value: "minio", label: "MinIO / 自建", endpoint: "https://", hint: "drive.providerMinioHint" },
];
const providerHint = computed(() => {
  const item = providers.find((candidate) => candidate.value === provider.value);
  return item ? t(item.hint) : "";
});
// 新增与编辑共用同一个弹窗：标题按是否带 id 区分，复用既有文案，不新增翻译。
const editorTitle = computed(() => (connection.value.id ? t('drive.connectionDetails') : t('drive.add')));
function fail(error) { app.handleFailure(error); }
function notifyConnectionsChanged() {
  if (typeof globalThis.dispatchEvent === "function" && typeof globalThis.CustomEvent === "function") globalThis.dispatchEvent(new CustomEvent("peerdav:cloud-changed"));
}
async function refresh() { try { data.value = await cloud.list(); } catch (error) { fail(error); } }
async function selectConnection(item) {
  connection.value = connectionEditor(item);
  connection.value.buckets = item?.buckets?.length ? [...item.buckets] : (item?.bucket ? [item.bucket] : []);
  url.value = item?.endpoint || "";
  buckets.value = item?.buckets?.length ? item.buckets.map((name) => ({ name })) : (item?.bucket ? [{ name: item.bucket }] : []);
  provider.value = ""; probing.value = false;
  advancedOpen.value = false; editorOpen.value = true;
  // Editing backfills AK/SK so the user can verify without re-entering secrets.
  if (item?.id) {
    try {
      const credentials = await cloud.connectionCredentials(String(item.id));
      if (connection.value.id === String(item.id)) {
        connection.value.accessKey = credentials.accessKey || "";
        connection.value.secretKey = credentials.secretKey || "";
        connection.value.sessionToken = credentials.sessionToken || "";
      }
    } catch (error) { fail(error); }
  }
}
function changeUrl(value) {
  url.value = value;
  const result = parseS3Url(value);
  connection.value.endpoint = result.endpoint;
  connection.value.region = result.region;
  // The bucket is never taken from the address; it is discovered by probing.
  connection.value.bucket = "";
  connection.value.buckets = [];
  buckets.value = [];
  // object_store requires bucket-bearing endpoints for virtual-hosted mode.
  // Our normalized service endpoints therefore use path-style requests.
  if (result.known) connection.value.pathStyle = true;
}
function applyProvider(value) {
  const item = providers.find((candidate) => candidate.value === value);
  if (item) { url.value = item.endpoint; changeUrl(item.endpoint); }
}
async function probe() {
  const endpoint = (connection.value.endpoint || url.value).trim();
  if (!endpoint || !connection.value.accessKey || !connection.value.secretKey) { app.setError("drive.probeRequired"); return; }
  probing.value = true;
  try {
    const result = await cloud.probeBuckets({
      endpoint,
      region: connection.value.region,
      accessKey: connection.value.accessKey,
      secretKey: connection.value.secretKey,
      sessionToken: connection.value.sessionToken || null,
      allowInsecureHttp: connection.value.allowInsecureHttp,
      proxy: connection.value.proxy,
    });
    buckets.value = result;
    if (!result.length) { app.setError("drive.noBuckets"); return; }
    const names = result.map((item) => item.name);
    const kept = connection.value.buckets.filter((name) => names.includes(name));
    connection.value.buckets = kept.length ? kept : [names[0]];
    connection.value.bucket = connection.value.buckets[0];
  } catch (error) { buckets.value = []; fail(error); }
  finally { probing.value = false; }
}
async function saveConnection() {
  if (!connection.value.endpoint) { app.setError("drive.probeRequired"); return; }
  if (!connection.value.buckets.length) { app.setError("drive.bucketRequired"); return; }
  connection.value.bucket = connection.value.buckets[0];
  loading.value = true;
  try {
    await cloud.saveConnection({ ...connection.value, accessKey: connection.value.accessKey || undefined, secretKey: connection.value.secretKey || undefined, sessionToken: connection.value.sessionToken || undefined });
    notifyConnectionsChanged();
    connection.value = blankConnection(); editorOpen.value = false; url.value = ""; buckets.value = []; provider.value = "";
    await refresh(); app.setNotice("cloud.saved");
  } catch (error) { fail(error); } finally { loading.value = false; }
}
async function testConnection(item) { if (!item?.id) return; try { await cloud.testConnection(String(item.id)); await refresh(); } catch (error) { fail(error); } }
function closeEditor() {
  editorOpen.value = false;
  connection.value = blankConnection();
  url.value = "";
  buckets.value = []; provider.value = ""; probing.value = false;
  advancedOpen.value = false;
}
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
    <!-- 连接列表对齐其它面板语法：组标题 + 计数徽章 + 右上角加号胶囊，列表用带边框的行式容器，
         每行名称在左、endpoint/bucket 作副行，操作收进右侧图标胶囊；无连接时给出空态与脚注。 -->
    <div class="settings-group">
      <div class="settings-group__head">
        <h3 class="group-title">{{ t('drive.connections') }}<span v-if="data.connections.length" class="count-badge">{{ data.connections.length }}</span></h3>
        <el-button class="list-pill pane-cta connection-add" type="primary" round size="small" :aria-label="t('drive.add')" @click="selectConnection()">
          <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-cta__icon"><path d="M12 5v14M5 12h14" /></svg>
        </el-button>
      </div>

      <div v-if="data.connections.length" class="settings-rows">
        <div v-for="item in data.connections" :key="item.id" class="settings-row cloud-connection">
          <div class="settings-row__text cloud-connection__text">
            <span class="settings-row__label">{{ item.name }}</span>
            <span class="hint mono">{{ item.endpoint }} / {{ item.bucket }}</span>
          </div>
          <div class="cloud-connection__actions">
            <el-button class="list-pill cloud-browse-entry" type="primary" round size="small" :aria-label="t('cloudBrowser.open')" @click="writeRoute({ view: 'cloud', connectionId: String(item.id) })">
              <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon">
                <path d="M4 7.5A1.5 1.5 0 0 1 5.5 6h3.3l1.7 1.9h6A1.5 1.5 0 0 1 18 9.4" />
                <path d="M4 10.2h15.1l-1.9 6.3a1.6 1.6 0 0 1-1.5 1.1H5.6A1.6 1.6 0 0 1 4 16Z" />
              </svg>
            </el-button>
            <el-button class="list-pill connection-edit" round size="small" :aria-label="t('cloud.edit')" @click="selectConnection(item)">
              <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon">
                <path d="M4 20h4L19.5 8.5a2.12 2.12 0 0 0-3-3L5 17z" />
                <path d="m14.5 6.5 3 3" />
              </svg>
            </el-button>
            <el-button class="list-pill connection-remove" round size="small" :aria-label="t('drive.remove')" @click="deleteConnection(item)">
              <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon">
                <path d="M4 7h16" />
                <path d="M9 7V4.5h6V7" />
                <path d="m6.5 7 .8 13h9.4l.8-13" />
                <path d="M10 11v5.5M14 11v5.5" />
              </svg>
            </el-button>
          </div>
        </div>
      </div>
      <div v-else class="cloud-empty"><p class="hint">{{ t('drive.connectionsEmpty') }}</p></div>

      <p class="settings-group__footnote">{{ t('drive.settingsHint') }}</p>
    </div>

    <el-dialog
      v-model="editorOpen"
      :title="editorTitle"
      width="520px"
      align-center
      :close-on-click-modal="false"
      :close-on-press-escape="false"
      destroy-on-close
      class="connection-dialog"
    >
      <form class="stack connection-editor connection-form" @submit.prevent="saveConnection">
        <label class="required"><span class="label">{{ t('cloud.name') }}</span><el-input v-model="connection.name" class="connection-name" /></label>
        <label><span class="label">{{ t('drive.provider') }}</span>
          <el-select v-model="provider" class="connection-provider" @change="applyProvider">
            <el-option value="" :label="t('drive.providerCustom')" />
            <el-option v-for="item in providers" :key="item.value" :value="item.value" :label="item.label" />
          </el-select>
        </label>
        <p v-if="providerHint" class="hint">{{ providerHint }}</p>
        <label class="required"><span class="label">{{ t('drive.url') }}</span><el-input :model-value="url" class="connection-url" @update:model-value="changeUrl" /></label>
        <p class="hint">{{ t('drive.urlHint') }}</p>
        <label class="required"><span class="label">{{ t('cloud.accessKey') }}</span><el-input v-model="connection.accessKey" type="password" autocomplete="off" class="connection-access-key" show-password /></label>
        <label class="required"><span class="label">{{ t('cloud.secretKey') }}</span><el-input v-model="connection.secretKey" type="password" autocomplete="new-password" class="connection-secret-key" show-password /></label>
        <div class="connection-probe">
          <el-button class="connection-probe-run" :loading="probing" :disabled="probing || !url.trim() || !connection.accessKey || !connection.secretKey" @click="probe">{{ t('drive.probeBuckets') }}</el-button>
          <span v-if="buckets.length" class="hint">{{ t('drive.bucketsFound', { count: buckets.length }) }}</span>
        </div>
        <label v-if="buckets.length" class="required"><span class="label">{{ t('cloud.bucket') }}</span>
          <el-select v-model="connection.buckets" class="connection-bucket" multiple :disabled="probing">
            <el-option v-for="item in buckets" :key="item.name" :value="item.name" :label="item.name" />
          </el-select>
        </label>
        <p v-if="buckets.length" class="hint">{{ t('drive.bucketMultiHint') }}</p>
        <details :open="advancedOpen" class="connection-advanced" @toggle="advancedOpen = $event.target.open"><summary>{{ t('drive.advanced') }}</summary>
          <div class="stack connection-advanced__fields">
            <label>{{ t("cloud.prefix") }}<el-input v-model="connection.prefix" /></label>
            <label>{{ t("cloud.sessionToken") }}<el-input v-model="connection.sessionToken" type="password" /></label>
            <label>{{ t("cloud.proxy") }}<el-select v-model="connection.proxy.mode"><el-option value="direct" :label="t('cloud.proxyDirect')" /><el-option value="environment" :label="t('cloud.proxyEnvironment')" /><el-option value="custom" :label="t('cloud.proxyCustom')" /></el-select></label>
            <template v-if="connection.proxy.mode === 'custom'"><label>{{ t("cloud.proxyUrl") }}<el-input v-model="connection.proxy.url" /></label><label>{{ t("cloud.proxyUsername") }}<el-input v-model="connection.proxy.username" /></label><label>{{ t("cloud.proxyPassword") }}<el-input v-model="connection.proxy.password" type="password" /></label></template>
            <label class="connection-switch">{{ t("cloud.allowInsecureHttp") }}<el-switch v-model="connection.allowInsecureHttp" /></label>
            <label class="connection-switch">{{ t("cloud.pathStyle") }}<el-switch v-model="connection.pathStyle" /></label>
            <label class="connection-switch">{{ t("cloud.clearCredentials") }}<el-switch v-model="connection.clearCredentials" /></label>
            <label class="connection-switch">{{ t("cloud.clearSessionToken") }}<el-switch v-model="connection.clearSessionToken" /></label>
            <label class="connection-switch">{{ t("cloud.clearProxyAuth") }}<el-switch v-model="connection.clearProxyAuth" /></label>
          </div>
        </details>
      </form>
      <template #footer>
        <el-button class="connection-cancel" plain @click="closeEditor">{{ t('confirm.cancel') }}</el-button>
        <el-button v-if="connection.id" :disabled="loading" @click="testConnection(connection)">{{ t('cloud.test') }}</el-button>
        <el-button class="connection-save" type="primary" :loading="loading" @click="saveConnection">{{ t('cloud.save') }}</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
/* 网盘页复用其它设置面板的分组 / 行式列表语法。这些原语在 SettingsView 里是 scoped，
   子组件拿不到，故在此自带一份（与 CloudSyncDrives 同模式），保证跨面板视觉一致。 */
.cloud-settings {
  display: flex;
  flex-direction: column;
  gap: 26px;
  min-width: 0;
}

.settings-rows {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
  overflow: hidden;
}

.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  min-width: 0;
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

.settings-row__text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.settings-group__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

.settings-group__head .group-title {
  margin: 0;
}

.settings-group__footnote {
  margin: 8px 2px 0;
  font-size: 12px;
  font-style: italic;
  line-height: 1.5;
  color: var(--app-text-secondary);
}

.pane-cta__icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
}

/* 连接行：名称 + endpoint/bucket 副行在左，操作图标胶囊在右。 */
.cloud-connection__text {
  flex: 1 1 auto;
}

.cloud-connection__actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
}

.cloud-connection__action-icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

/* 空态与其它面板的列表空态同构：描边圆角盒 + 提示。 */
.cloud-empty {
  box-sizing: border-box;
  width: 100%;
  padding: 14px 15px;
  border: 1px solid var(--app-border);
  border-radius: 8px;
  background: var(--app-surface);
}

.cloud-empty .hint {
  margin: 0;
}

/* 高级设置折叠：在纵向堆叠表单里作为一段可展开的次级分组。 */
.connection-advanced {
  border-top: 1px solid var(--app-border);
  padding-top: 14px;
}

.connection-advanced > summary {
  display: flex;
  align-items: center;
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text-secondary);
  list-style: none;
  margin-bottom: 14px;
}

.connection-advanced > summary::-webkit-details-marker {
  display: none;
}

.connection-advanced > summary::before {
  content: "+";
  margin-inline-end: 8px;
  font-weight: 700;
  color: var(--app-text-muted);
}

.connection-advanced[open] > summary::before {
  content: "\2212";
}

/* 开关行：标签在左、开关在右，覆盖全局 .stack > label 的纵向布局。 */
.connection-switch {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

/* 弹窗表单：下拉框铺满列宽，与输入框对齐。 */
.connection-form :deep(.el-select) {
  width: 100%;
}

/* 「测试并获取存储桶」：按钮与结果提示同一行，结果过长时换行。 */
.connection-probe {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.connection-probe .hint {
  margin: 0;
}

/* 必填标记：红色星号紧跟标签文本，与 Element Plus 表单一致。 */
.connection-form label.required > .label::after {
  content: "*";
  margin-inline-start: 4px;
  color: var(--el-color-danger, #f56c6c);
}

/* 对话框始终在标题栏下方居中：覆盖层用 flex 居中，弹窗高度贴合内容，
   收起「高级设置」时不会撑满窗口；展开后超过 max-height 才由主体滚动，
   主体两侧预留 15px gutter，出现滚动条时不会遮住表单内容。 */
:deep(.el-overlay-dialog) {
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  padding-top: var(--titlebar-height, 42px);
  padding-bottom: 12px;
}

:deep(.el-dialog) {
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  max-height: calc(100vh - var(--titlebar-height, 42px) - 20px);
  margin: 0 auto;
  padding: 0;
}

:deep(.el-dialog__header) {
  box-sizing: border-box;
  margin: 0;
  padding: 18px 15px 10px;
}

:deep(.el-dialog__body) {
  box-sizing: border-box;
  flex: 1 1 auto;
  overflow-y: auto;
  padding: 6px 15px;
}

:deep(.el-dialog__footer) {
  box-sizing: border-box;
  padding: 10px 15px 18px;
}

@media (max-width: 480px) {
  .cloud-connection {
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .cloud-connection__actions {
    flex-wrap: wrap;
  }
}
</style>
