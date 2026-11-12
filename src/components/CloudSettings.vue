<script setup>
import { computed, h, onBeforeUnmount, onMounted, ref } from "vue";
import { cloud, blankConnection } from "../lib/cloud";
import { connectionEditor, parseS3Url } from "../lib/s3Url";
import { confirmAction } from "../lib/confirm";
import { useAppContext } from "../lib/appContext";
import { t, locale } from "../lib/i18n";
import { writeRoute } from "../lib/navigation";

const app = useAppContext();
const data = ref({ connections: [], mappings: [] });
// 两个折叠列表的展开状态；官方网盘列表归一到 computed，避免重复判空。
const activeOfficialIds = ref([]);
const activeConnectionIds = ref([]);
const officialConnections = computed(() => data.value.officialConnections || []);
const connection = ref(blankConnection());
const editorOpen = ref(false);
const advancedOpen = ref(false);
const url = ref("");
const loading = ref(false);
const probing = ref(false);
const buckets = ref([]);
const officialOpen = ref(false);
const protocolDescriptors = ref([]);
// 官方网盘全部开放；OneDrive 已补齐上传与同步能力。
const hiddenOfficialProtocolIds = [];
const officialDescriptors = computed(() => protocolDescriptors.value.filter(item => item.auth === 'oauth2' && !hiddenOfficialProtocolIds.includes(item.id)));
const officialFields = computed(() => officialDescriptors.value.find(item => item.id === official.value.provider)?.fields || []);
async function openOfficial() {
  try {
    protocolDescriptors.value = await cloud.protocols();
    if (!officialDescriptors.value.some(item => item.id === official.value.provider)) official.value.provider = officialDescriptors.value[0]?.id || '';
    if (official.value.provider) {
      // 新建对话框打开即按当前协议填充名称。
      official.value.name = officialNameForProvider(official.value.provider);
      officialOpen.value = true;
    }
  } catch (error) { fail(error); }
}
const officialBusy = ref(false);
const officialEdit = ref(null);
let officialEditVersion = 0;
async function editOfficial(item) {
  const version = ++officialEditVersion;
  try {
    const direct = await cloud.driveProxy(item.id);
    if (version !== officialEditVersion) return;
    officialEdit.value = { id: item.id, revision: item.revision, name: item.name, protocolId: item.protocolId, rootId: item.rootId, clientSecret: '', clearClientSecret: false, direct };
  } catch (error) { fail(error); }
}
function closeOfficialEdit() { officialEditVersion++; officialEdit.value = null; }
async function saveOfficialEdit() {
  if (!officialEdit.value || officialBusy.value) return;
  officialBusy.value = true;
  try {
    await cloud.updateOfficialConnection({ ...officialEdit.value, clientSecret: officialEdit.value.clientSecret || null });
    closeOfficialEdit(); notifyConnectionsChanged(); await refresh(); app.setNotice('cloud.saved');
  } catch (error) { fail(error); }
  finally { officialBusy.value = false; }
}
const authorization = ref(null);
const official = ref({ provider: 'google_drive', name: '', clientId: '', clientSecret: '', redirectUri: 'http://127.0.0.1:53682/oauth/callback', rootId: 'root', callback: '', direct: false });
const loopbackWaiting = ref(false);
let authorizationVersion = 0;
function officialNameForProvider(id) {
  return officialDescriptors.value.find(item => item.id === id)?.name || '';
}
function changeOfficialProvider() {
  const roots = { baidu_netdisk: '/', dropbox: '/', yandex_disk: 'disk:/' };
  official.value.rootId = roots[official.value.provider] ?? 'root';
  // 名称随协议自动填充为协议显示名（如 Google Drive / 百度网盘），用户可再改。
  official.value.name = officialNameForProvider(official.value.provider);
}
async function cancelOfficial() {
  authorizationVersion++;
  loopbackWaiting.value = false;
  const request = authorization.value;
  authorization.value = null;
  official.value.clientSecret = ''; official.value.callback = ''; official.value.name = '';
  officialOpen.value = false;
  if (request) { try { await cloud.cancelOfficialAuthorization(request.requestId); } catch (error) { fail(error); } }
}
async function beginOfficial() {
  const version = ++authorizationVersion;
  officialBusy.value = true;
  try {
    if (authorization.value) await cloud.cancelOfficialAuthorization(authorization.value.requestId);
    authorization.value = null;
    const loopback = official.value.provider !== 'baidu_netdisk';
    const result = await (loopback ? cloud.beginLoopbackAuthorization : cloud.beginOfficialAuthorization)(official.value);
    if (version !== authorizationVersion) { await cloud.cancelOfficialAuthorization(result.requestId); return; }
    authorization.value = result;
    if (loopback) void waitOfficial(result, version);
  } catch (error) { fail(error); }
  finally { officialBusy.value = false; }
}
async function waitOfficial(request, version) {
  loopbackWaiting.value = true;
  try {
    await cloud.waitLoopbackAuthorization({ requestId: request.requestId, name: official.value.name, clientSecret: official.value.clientSecret || null, rootId: official.value.rootId, direct: official.value.direct });
    if (version !== authorizationVersion) return;
    authorization.value = null; officialOpen.value = false;
    official.value.clientSecret = ''; official.value.callback = '';
    notifyConnectionsChanged(); await refresh();
  } catch (error) {
    if (version === authorizationVersion) { authorization.value = null; fail(error); }
  } finally { if (version === authorizationVersion) loopbackWaiting.value = false; }
}
async function finishOfficial() {
  if (loopbackWaiting.value || !authorization.value) return;
  officialBusy.value = true;
  try {
    await cloud.completeOfficialAuthorization({ ...official.value, requestId: authorization.value.requestId });
    authorization.value = null;
    official.value.clientSecret = ''; official.value.callback = '';
    officialOpen.value = false;
    notifyConnectionsChanged(); await refresh();
  } catch (error) { authorization.value = null; fail(error); }
  finally { officialBusy.value = false; }
}
onBeforeUnmount(() => { closeOfficialEdit(); void cancelOfficial(); });
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
  if (item) {
    // 选择厂商即以其显示名作为名称默认值（用户可改）；编辑不触发此处，保留用户已存名称。
    connection.value.name = item.label;
    url.value = item.endpoint;
    changeUrl(item.endpoint);
    // MinIO/自建通常使用内网 http 端点，默认勾选允许 HTTP；切换其他厂商恢复安全默认。
    connection.value.allowInsecureHttp = value === "minio";
    // MinIO/自建无法使用 bucket 子域名虚拟主机寻址，默认改用路径寻址。
    if (value === "minio") connection.value.pathStyle = true;
  }
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
  const message = h("div", [h("p", { class: "confirm-summary" }, t("drive.removeConfirm", { name: item.name })), ...(count ? [h("p", { class: "confirm-warn", "data-test": "sync-removal-warning" }, t("drive.removeWarning", { count }))] : [])]);
  if (!await confirmAction(message, t("drive.remove"), { danger: true, confirmText: t("drive.remove"), type: count ? "error" : "warning" })) return;
  try { await cloud.deleteConnection(String(item.id)); notifyConnectionsChanged(); if (connection.value.id === item.id) { connection.value = blankConnection(); editorOpen.value = false; } await refresh(); } catch (error) { fail(error); }
}
onMounted(refresh);
</script>
<template>
  <div class="cloud-settings">
    <!-- 两个分组各用「图标 + 组标题 + 计数徽章 + 右上角加号」的标题行，列表复用设备/同步盘
         同款 client-collapse 折叠卡片：折叠头放名称与右侧图标操作，连接详情收进展开体。
         「网盘」走官方 OAuth 连接，「对象存储」走 S3 兼容连接，分别由各自加号打开对应表单。 -->
    <section class="client-role">
      <header class="role-heading">
        <span class="role-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24"><path d="M6.5 18a4 4 0 0 1-.6-7.96 5.5 5.5 0 0 1 10.7-1.5A4 4 0 0 1 17.5 18Z" /></svg>
        </span>
        <h3 class="group-title">{{ t('drive.connections') }}<span v-if="officialConnections.length" class="count-badge">{{ officialConnections.length }}</span></h3>
        <el-button class="list-pill pane-cta official-add" type="primary" round size="small" :aria-label="t('drive.officialAdd')" @click="openOfficial">
          <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-cta__icon"><path d="M12 5v14M5 12h14" /></svg>
        </el-button>
      </header>

      <el-collapse v-if="officialConnections.length" v-model="activeOfficialIds" class="client-collapse">
        <el-collapse-item v-for="item in officialConnections" :key="item.id" :name="item.id">
          <template #title>
            <div class="client-item__main">
              <span class="client-item__icon" role="img" :aria-label="item.protocolId">
                <svg viewBox="0 0 24 24"><path d="M6.5 18a4 4 0 0 1-.6-7.96 5.5 5.5 0 0 1 10.7-1.5A4 4 0 0 1 17.5 18Z" /></svg>
              </span>
              <div class="client-item__title">
                <strong>{{ item.name }}</strong>
                <span class="hint">{{ item.protocolId }}</span>
              </div>
            </div>
            <div class="client-item__actions" @click.stop>
              <el-tooltip :content="t('drive.editDriveInfo')" placement="top" :show-after="200">
                <el-button class="list-pill official-edit" round size="small" :aria-label="t('drive.editDriveInfo')" data-test="official-edit" @click="editOfficial(item)">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 20h4L19.5 8.5a2.12 2.12 0 0 0-3-3L5 17z" /><path d="m14.5 6.5 3 3" /></svg>
                </el-button>
              </el-tooltip>
              <el-tooltip :content="t('cloudBrowser.open')" placement="top" :show-after="200">
                <el-button class="list-pill official-browse-entry" type="primary" round size="small" :aria-label="t('cloudBrowser.open')" @click="writeRoute({ view: 'cloud', connectionId: String(item.id) })">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 7.5A1.5 1.5 0 0 1 5.5 6h3.3l1.7 1.9h6A1.5 1.5 0 0 1 18 9.4" /><path d="M4 10.2h15.1l-1.9 6.3a1.6 1.6 0 0 1-1.5 1.1H5.6A1.6 1.6 0 0 1 4 16Z" /></svg>
                </el-button>
              </el-tooltip>
              <el-tooltip :content="t('drive.remove')" placement="top" :show-after="200">
                <el-button class="list-pill official-remove connection-remove is-danger" round size="small" :aria-label="t('drive.remove')" @click="deleteConnection(item)">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 7h16" /><path d="M9 7V4.5h6V7" /><path d="m6.5 7 .8 13h9.4l.8-13" /><path d="M10 11v5.5M14 11v5.5" /></svg>
                </el-button>
              </el-tooltip>
            </div>
          </template>
          <ul class="client-item__detail">
            <li><span>{{ t('drive.protocol') }}</span><strong>{{ item.protocolId }}</strong></li>
            <li><span>{{ t('drive.rootId') }}</span><strong class="mono">{{ item.rootId }}</strong></li>
          </ul>
        </el-collapse-item>
      </el-collapse>
      <div v-else class="client-empty">
        <span class="client-empty__icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M6.5 18a4 4 0 0 1-.6-7.96 5.5 5.5 0 0 1 10.7-1.5A4 4 0 0 1 17.5 18Z" /></svg></span>
        <div><strong>{{ t('drive.officialAdd') }}</strong><p class="hint">{{ t('drive.connectionsEmpty') }}</p></div>
      </div>
      <p class="settings-group__footnote">{{ t('drive.settingsHint') }}</p>
    </section>

    <section class="client-role">
      <header class="role-heading">
        <span class="role-icon role-icon--storage" aria-hidden="true">
          <svg viewBox="0 0 24 24"><ellipse cx="12" cy="6" rx="7" ry="3" /><path d="M5 6v12c0 1.66 3.13 3 7 3s7-1.34 7-3V6" /><path d="M5 12c0 1.66 3.13 3 7 3s7-1.34 7-3" /></svg>
        </span>
        <h3 class="group-title">{{ t('drive.objectStorage') }}<span v-if="data.connections.length" class="count-badge">{{ data.connections.length }}</span></h3>
        <el-button class="list-pill pane-cta connection-add" type="primary" round size="small" :aria-label="t('drive.add')" @click="selectConnection()">
          <svg viewBox="0 0 24 24" aria-hidden="true" class="pane-cta__icon"><path d="M12 5v14M5 12h14" /></svg>
        </el-button>
      </header>

      <el-collapse v-if="data.connections.length" v-model="activeConnectionIds" class="client-collapse">
        <el-collapse-item v-for="item in data.connections" :key="item.id" :name="item.id">
          <template #title>
            <div class="client-item__main">
              <span class="client-item__icon" role="img" :aria-label="t('drive.objectStorage')">
                <svg viewBox="0 0 24 24"><ellipse cx="12" cy="6" rx="7" ry="3" /><path d="M5 6v12c0 1.66 3.13 3 7 3s7-1.34 7-3V6" /><path d="M5 12c0 1.66 3.13 3 7 3s7-1.34 7-3" /></svg>
              </span>
              <div class="client-item__title">
                <strong>{{ item.name }}</strong>
                <span class="hint mono">{{ item.endpoint }} / {{ item.bucket }}</span>
              </div>
            </div>
            <div class="client-item__actions" @click.stop>
              <el-tooltip :content="t('cloudBrowser.open')" placement="top" :show-after="200">
                <el-button class="list-pill cloud-browse-entry" type="primary" round size="small" :aria-label="t('cloudBrowser.open')" @click="writeRoute({ view: 'cloud', connectionId: String(item.id) })">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 7.5A1.5 1.5 0 0 1 5.5 6h3.3l1.7 1.9h6A1.5 1.5 0 0 1 18 9.4" /><path d="M4 10.2h15.1l-1.9 6.3a1.6 1.6 0 0 1-1.5 1.1H5.6A1.6 1.6 0 0 1 4 16Z" /></svg>
                </el-button>
              </el-tooltip>
              <el-tooltip :content="t('cloud.edit')" placement="top" :show-after="200">
                <el-button class="list-pill connection-edit" round size="small" :aria-label="t('cloud.edit')" @click="selectConnection(item)">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 20h4L19.5 8.5a2.12 2.12 0 0 0-3-3L5 17z" /><path d="m14.5 6.5 3 3" /></svg>
                </el-button>
              </el-tooltip>
              <el-tooltip :content="t('drive.remove')" placement="top" :show-after="200">
                <el-button class="list-pill connection-remove is-danger" round size="small" :aria-label="t('drive.remove')" @click="deleteConnection(item)">
                  <svg viewBox="0 0 24 24" aria-hidden="true" class="cloud-connection__action-icon"><path d="M4 7h16" /><path d="M9 7V4.5h6V7" /><path d="m6.5 7 .8 13h9.4l.8-13" /><path d="M10 11v5.5M14 11v5.5" /></svg>
                </el-button>
              </el-tooltip>
            </div>
          </template>
          <ul class="client-item__detail">
            <li><span>{{ t('drive.url') }}</span><strong class="mono">{{ item.endpoint }}</strong></li>
            <li><span>{{ t('cloud.bucket') }}</span><strong class="mono">{{ item.buckets?.length ? item.buckets.join('、') : item.bucket }}</strong></li>
          </ul>
        </el-collapse-item>
      </el-collapse>
      <div v-else class="client-empty">
        <span class="client-empty__icon" aria-hidden="true"><svg viewBox="0 0 24 24"><ellipse cx="12" cy="6" rx="7" ry="3" /><path d="M5 6v12c0 1.66 3.13 3 7 3s7-1.34 7-3V6" /><path d="M5 12c0 1.66 3.13 3 7 3s7-1.34 7-3" /></svg></span>
        <div><strong>{{ t('drive.add') }}</strong><p class="hint">{{ t('drive.connectionsEmpty') }}</p></div>
      </div>
    </section>

    <el-dialog v-model="officialOpen" width="520px" :close-on-click-modal="false" :close-on-press-escape="false" :show-close="false" destroy-on-close>
      <template #header="{ titleId, titleClass }">
        <div class="create-header">
          <span :id="titleId" :class="titleClass">{{ t('drive.officialAdd') }}</span>
          <el-popover placement="bottom-start" trigger="click" :width="340" :teleported="false" popper-class="queue-help-popper">
            <ol class="queue-help"><li>{{ t('drive.officialHelp1') }}</li><li>{{ t('drive.officialHelp2') }}</li><li>{{ t('drive.officialHelp3') }}</li></ol>
            <template #reference>
              <button type="button" class="create-header__help" :aria-label="t('drive.officialHelpLabel')">?</button>
            </template>
          </el-popover>
        </div>
      </template>
      <form class="stack" @submit.prevent="authorization ? finishOfficial() : beginOfficial()">
        <label>{{ t('drive.protocol') }}<el-select v-model="official.provider" :disabled="officialBusy || !!authorization" @change="changeOfficialProvider"><el-option v-for="item in officialDescriptors" :key="item.id" :value="item.id" :label="item.name" /></el-select></label>
        <label>{{ t('cloud.name') }}<el-input v-model="official.name" :disabled="officialBusy || loopbackWaiting" /></label>
        <label v-for="field in officialFields" :key="field.key">{{ locale === 'zh-CN' ? field.labelZh : field.labelEn }}<el-input v-model="official[field.key]" :type="field.secret ? 'password' : 'text'" :autocomplete="field.secret ? 'new-password' : 'off'" :disabled="officialBusy || loopbackWaiting || (!!authorization && ['clientId', 'redirectUri'].includes(field.key))" /></label>
        <p v-if="official.provider === 'baidu_netdisk'" class="hint" data-test="baidu-credentials-hint">{{ t('drive.baiduCredentialsHint') }}</p>
        <label>{{ t('network.title') }}<el-select v-model="official.direct" data-test="official-proxy" :disabled="officialBusy || !!authorization"><el-option :value="false" :label="t('network.client')" /><el-option :value="true" :label="t('cloud.proxyDirect')" /></el-select></label>
        <p class="hint">{{ t('drive.officialProxyHint') }}</p>
        <p v-if="loopbackWaiting" class="hint" role="status">{{ t('drive.waitingAuthorization') }}</p>
        <template v-if="authorization">
          <a :href="authorization.authorizationUrl" target="_blank" rel="noopener noreferrer">{{ t('drive.openAuthorization') }}</a>
          <label v-if="!loopbackWaiting">{{ t('drive.callback') }}<el-input v-model="official.callback" type="password" autocomplete="off" :disabled="officialBusy" /></label>
        </template>
      </form>
      <template #footer>
        <el-button :disabled="officialBusy" @click="cancelOfficial">{{ t('confirm.cancel') }}</el-button>
        <el-button v-if="!authorization" type="primary" :loading="officialBusy" :disabled="!official.clientId || !official.redirectUri || !official.name" @click="beginOfficial">{{ t('drive.authorize') }}</el-button>
        <el-button v-else-if="!loopbackWaiting" type="primary" :loading="officialBusy" :disabled="!official.callback" @click="finishOfficial">{{ t('drive.finishAuthorization') }}</el-button>
      </template>
    </el-dialog>
    <el-dialog :model-value="!!officialEdit" :title="t('drive.connectionDetails')" width="520px" :show-close="false" :close-on-click-modal="false" :close-on-press-escape="false" destroy-on-close>
      <form v-if="officialEdit" class="stack official-editor" @submit.prevent="saveOfficialEdit">
        <label>{{ t('drive.protocol') }}<el-input :model-value="officialEdit.protocolId" disabled /></label>
        <label>{{ t('cloud.name') }}<el-input v-model="officialEdit.name" data-test="official-name" :disabled="officialBusy" /></label>
        <label>{{ t('drive.rootId') }}<el-input :model-value="officialEdit.rootId" disabled /></label>
        <p class="hint">{{ t('drive.officialEditHint') }}</p>
        <label>{{ t('drive.clientSecret') }}<el-input v-model="officialEdit.clientSecret" data-test="official-secret" type="password" autocomplete="new-password" :placeholder="t('drive.keepSecretHint')" :disabled="officialBusy || officialEdit.clearClientSecret" /></label>
        <label>{{ t('drive.clearClientSecret') }}<el-switch v-model="officialEdit.clearClientSecret" :disabled="officialBusy" /></label>
        <label>{{ t('network.title') }}<el-select v-model="officialEdit.direct" data-test="official-edit-proxy" :disabled="officialBusy"><el-option :value="false" :label="t('network.client')" /><el-option :value="true" :label="t('cloud.proxyDirect')" /></el-select></label>
        <p class="hint">{{ t('drive.officialProxyHint') }}</p>
      </form>
      <template #footer><el-button :disabled="officialBusy" @click="closeOfficialEdit">{{ t('confirm.cancel') }}</el-button><el-button data-test="official-save" type="primary" :loading="officialBusy" :disabled="!officialEdit?.name.trim()" @click="saveOfficialEdit">{{ t('cloud.save') }}</el-button></template>
    </el-dialog>
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
        <label><span class="label">{{ t('drive.provider') }}</span>
          <el-select v-model="provider" class="connection-provider" @change="applyProvider">
            <el-option value="" :label="t('drive.providerCustom')" />
            <el-option v-for="item in providers" :key="item.value" :value="item.value" :label="item.label" />
          </el-select>
        </label>
        <p v-if="providerHint" class="hint">{{ providerHint }}</p>
        <label class="required"><span class="label">{{ t('cloud.name') }}</span><el-input v-model="connection.name" class="connection-name" /></label>
        <label class="required"><span class="label">{{ t('drive.url') }}</span>
          <span class="connection-url-row">
            <el-input :model-value="url" class="connection-url" @update:model-value="changeUrl" />
            <span class="connection-inline-switch">
              <el-switch v-model="connection.allowInsecureHttp" :aria-label="t('cloud.allowInsecureHttp')" />
              <span>{{ t('cloud.allowInsecureHttp') }}</span>
            </span>
          </span>
        </label>
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
            <label>{{ t("network.title") }}<el-select v-model="connection.proxy.mode" :aria-label="t('network.title')"><el-option value="client" :label="t('network.client')" /><el-option value="direct" :label="t('cloud.proxyDirect')" /></el-select></label>
            <template v-if="connection.proxy.mode === 'custom'"><label>{{ t("cloud.proxyUrl") }}<el-input v-model="connection.proxy.url" /></label><label>{{ t("cloud.proxyUsername") }}<el-input v-model="connection.proxy.username" /></label><label>{{ t("cloud.proxyPassword") }}<el-input v-model="connection.proxy.password" type="password" /></label></template>
            <label class="connection-switch">{{ t("cloud.pathStyle") }}<el-switch v-model="connection.pathStyle" data-test="connection-path-style" /></label>
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
/* 网盘页复用设备 / 同步盘列表的折叠卡片语法。这些原语在 SettingsView 里是 scoped，
   子组件拿不到，故在此自带一份（与 CloudSyncDrives 同模式），保证跨面板视觉一致。 */
.cloud-settings {
  display: flex;
  flex-direction: column;
  gap: 26px;
  min-width: 0;
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

/* 折叠头右侧图标胶囊内的描边图标。 */
.cloud-connection__action-icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}

/* 分组标题行：图标 + 标题 + 计数 + 右上角加号，几何与设备/同步盘列表一致。 */
.client-role {
  min-width: 0;
}

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

.role-icon--storage {
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

/* 折叠列表：外框描边裁圆、行分隔自绘、折叠头两栏对齐（标识 / 动作）。 */
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
  grid-template-columns: minmax(0, 1fr) auto;
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

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

/* 危险操作（移除）图标用红色，与设备/同步盘列表的中性胶囊区分。 */
.list-pill.is-danger {
  color: var(--el-color-danger, #f56c6c);
  border-color: color-mix(in oklab, var(--el-color-danger, #f56c6c) 45%, var(--app-border));
}

.list-pill.is-danger:hover {
  color: var(--el-color-danger, #f56c6c);
  border-color: var(--el-color-danger, #f56c6c);
  background: color-mix(in oklab, var(--el-color-danger, #f56c6c) 8%, transparent);
}

/* 空态与折叠面板同款外框，保证同一套设计语言。 */
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

/* 对话框标题行：标题 + 右侧「?」帮助，点击展开有序列表说明。 */
.create-header {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.create-header__help {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: 1px solid var(--app-border);
  border-radius: 50%;
  background: transparent;
  color: var(--app-text-muted);
  font-size: 11px;
  line-height: 1;
  cursor: pointer;
}

.create-header__help:hover,
.create-header__help:focus-visible {
  color: var(--app-text);
  border-color: var(--app-text-muted);
  outline: none;
}

.queue-help {
  margin: 0;
  padding-inline-start: 1.1rem;
  max-width: 320px;
  font-size: 12.5px;
  line-height: 1.55;
  color: var(--app-text-secondary);
}

.queue-help li {
  margin: 0 0 6px;
}

.queue-help li:last-child {
  margin-bottom: 0;
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

/* 地址行：允许 HTTP 与地址输入框同行，窄窗口下输入框先收缩。 */
.connection-url-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.connection-url-row .connection-url {
  flex: 1 1 0;
  min-width: 0;
}

.connection-inline-switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex: 0 0 auto;
  font-weight: 600;
  color: var(--app-text-strong);
  white-space: nowrap;
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
</style>
