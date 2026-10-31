<script setup>
import { computed, h, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { cloud, cloudObjects, objectChildren } from "../lib/cloud";
import { confirmAction } from "../lib/confirm";
import { writeRoute } from "../lib/navigation";
import { t } from "../lib/i18n";

const props = defineProps({ connectionId: { type: String, required: true } });
const connections = ref([]);
const prefix = ref("");
const root = ref("");
const objects = ref([]);
const next = ref(null);
const complete = ref(false);
const loading = ref(false);
const busy = ref(false);
const error = ref(null);
const protectedRoot = ref(false);
const identified = ref(false);
const discovered = ref(false);
const association = ref("new");
const physicalId = ref("");
const uploadName = ref("");
const transfers = ref([]);
const partial = ref(null);
let disposed = false;
let requestId = 0;
let statusVersion = 0;
let polling = false;
let timer;
const children = computed(() => objectChildren(objects.value, prefix.value));
const hasProtectedObjects = computed(() => objects.value.some((item) => item.protected === true));
const targetProtected = (target) => protectedRoot.value || objects.value.some((item) => item.protected === true && item.key.startsWith(target));
const canMutate = computed(() => identified.value && discovered.value && !protectedRoot.value && !busy.value && !loading.value);
const current = () => ({ id: requestId, connectionId: props.connectionId, prefix: prefix.value });
const valid = (scope) => !disposed && scope.id === requestId && scope.connectionId === props.connectionId && scope.prefix === prefix.value;
function fail(cause, scope) {
  if (!valid(scope)) return;
  error.value = cause;
  if (cause?.code === "CLOUD_OBJECT_PROTECTED") { protectedRoot.value = true; discovered.value = false; }
  if (["CLOUD_PHYSICAL_IDENTITY_UNCONFIRMED", "CLOUD_PHYSICAL_BINDING_INVALID"].includes(cause?.code)) { identified.value = false; discovered.value = false; }
  if (cause?.code === "CLOUD_DISCOVERY_INCOMPLETE") discovered.value = false;
}
async function load(more = false) {
  if (more && (loading.value || !next.value)) return;
  const scope = current();
  const cursor = more ? next.value : null;
  loading.value = true; error.value = null;
  try {
    const page = await cloudObjects.list({ connectionId: scope.connectionId, prefix: scope.prefix, next: cursor });
    if (!valid(scope)) return;
    objects.value = [...new Map([...(more ? objects.value : []), ...page.objects].map((item) => [item.key, item])).values()];
    protectedRoot.value = protectedRoot.value || page.prefixProtected === true;
    next.value = page.next; complete.value = page.complete === true && page.next == null;
  } catch (cause) { fail(cause, scope); }
  finally { if (valid(scope)) loading.value = false; }
}
function navigate(value) {
  if (!value.startsWith(root.value)) return;
  requestId++; prefix.value = value; objects.value = []; next.value = null; complete.value = false;
  discovered.value = false; protectedRoot.value = false; busy.value = false; partial.value = null;
  void load();
}
function up() { const path = prefix.value.slice(root.value.length); const relative = path.endsWith("/") ? path.slice(0, -1) : path; const slash = relative.lastIndexOf("/"); navigate(root.value + (slash < 0 ? "" : relative.slice(0, slash + 1))); }
async function operate(action) {
  if (busy.value) return;
  const scope = current(); busy.value = true; error.value = null;
  try { await action(scope); } catch (cause) { fail(cause, scope); }
  finally { if (valid(scope)) busy.value = false; }
}
async function prepare() {
  if (busy.value || loading.value || protectedRoot.value) return;
  identified.value = false; discovered.value = false;
  await operate(async (scope) => {
    if (association.value === "existing" && !physicalId.value.trim()) return;
    const result = await cloudObjects.registerIdentity({ connectionId: scope.connectionId, association: association.value === "existing" ? { kind: "existing", physicalId: physicalId.value.trim() } : { kind: "new" } });
    if (!valid(scope)) return;
    identified.value = Boolean(result.physicalId);
    if (!identified.value) return;
    const scan = await cloudObjects.discover({ connectionId: scope.connectionId, prefix: scope.prefix });
    if (valid(scope)) discovered.value = scan.complete === true;
  });
}
function remember(transfer, scope) {
  if (!valid(scope)) return;
  statusVersion++;
  transfers.value = [...transfers.value.filter((item) => item.transferId !== transfer.transferId), transfer];
  if (transfer.error) fail(transfer.error, scope);
}
async function upload() {
  if (!canMutate.value) return;
  await operate(async (scope) => {
    const selected = await cloudObjects.selectLocal({ connectionId: scope.connectionId, direction: "upload" });
    if (!valid(scope) || !selected?.token) return;
    const key = scope.prefix + (uploadName.value || selected.fileName);
    const existing = objects.value.find((item) => item.key === key);
    if (existing?.protected === true || protectedRoot.value) return;
    if (existing && (!existing.etag || !await confirmAction(t("cloudBrowser.uploadOverwrite", { key, etag: existing.etag }), t("cloudBrowser.upload")))) return;
    if (!valid(scope)) return;
    remember(await cloudObjects.upload({ connectionId: scope.connectionId, localToken: selected.token, key, overwriteEtag: existing?.etag ?? null, overwriteConfirmed: Boolean(existing) }), scope);
  });
}
const downloadTargets = ref({});
async function retryDownload(item) {
  const scope = current();
  if (!await confirmAction(t("cloudBrowser.downloadRetry"), t("cloudBrowser.download")) || !valid(scope)) return;
  await download(item, true);
}
async function download(item, confirmed = false) {
  await operate(async (scope) => {
    const args = { connectionId: scope.connectionId, direction: "download" };
    let selected = await cloudObjects.selectLocal(args);
    if (!valid(scope) || !selected?.token) return;
    const run = (overwriteConfirmed) => cloudObjects.download({ connectionId: scope.connectionId, localToken: selected.token, key: item.key, fileName: item.name, overwriteConfirmed });
    const accept = (transfer) => { if (valid(scope)) downloadTargets.value[transfer.transferId] = { ...item }; remember(transfer, scope); };
    try { accept(await run(confirmed)); }
    catch (cause) {
      if (confirmed || cause.code !== "CLOUD_PRECONDITION" || !valid(scope)) throw cause;
      fail(cause, scope);
      if (!await confirmAction(t("cloudBrowser.downloadRetry"), t("cloudBrowser.download")) || !valid(scope)) return;
      selected = await cloudObjects.selectLocal(args);
      if (!valid(scope) || !selected?.token) return;
      error.value = null; accept(await run(true));
    }
  });
}
function validDeletePlan(plan, target) {
  return plan != null && Number.isSafeInteger(plan.objectCount) && plan.objectCount >= 0 && Array.isArray(plan.entries)
    && plan.entries.length === plan.objectCount && new Set(plan.entries.map((item) => item?.key)).size === plan.objectCount
    && plan.entries.every((item) => typeof item?.key === "string" && item.key.startsWith(target)
      && !/[\\\0:]/.test(item.key) && item.key.split("/").every((part) => part && part !== "." && part !== "..")
      && Number.isSafeInteger(item.size) && item.size >= 0 && (item.etag === null || typeof item.etag === "string"))
    && typeof plan.planId === "string" && plan.planId.length > 0 && Number.isFinite(plan.expiresAtUnix);
}
async function remove(target) {
  if (!canMutate.value || targetProtected(target)) return;
  await operate(async (scope) => {
    partial.value = null;
    const plan = await cloudObjects.deletePlan({ connectionId: scope.connectionId, prefix: target });
    if (!valid(scope)) return;
    if (!validDeletePlan(plan, target)) { error.value = { code: "CLOUD_CONFIRMATION_REQUIRED", message: t("cloudBrowser.invalidPlan") }; return; }
    const accepted = await confirmAction(h("div", [
      h("p", { style: { whiteSpace: "pre-wrap" } }, t("cloudBrowser.deleteConfirm", { prefix: JSON.stringify(target), count: plan.objectCount })),
      h("ul", { "data-test": "delete-keys", style: { maxHeight: "40vh", overflow: "auto", paddingLeft: "20px", overflowWrap: "anywhere", whiteSpace: "pre-wrap" } }, plan.entries.map((item) => h("li", { key: item.key }, JSON.stringify(item.key)))),
    ]), t("cloudBrowser.delete"));
    if (!accepted || !valid(scope)) return;
    if (Date.now() >= plan.expiresAtUnix * 1000) { error.value = { code: "CLOUD_DELETE_PLAN_STALE", message: t("cloudBrowser.stalePlan") }; return; }
    const result = await cloudObjects.deleteExecute({ connectionId: scope.connectionId, planId: plan.planId, confirmed: true });
    if (!valid(scope)) return;
    partial.value = result;
    for (const item of result.failed) if (item.error?.code === "CLOUD_OBJECT_PROTECTED") fail(item.error, scope);
    await load();
  });
}
async function acknowledge(item) {
  if (item.phase !== "unknown" || busy.value) return;
  await operate(async (scope) => {
    if (!await confirmAction(t("cloudBrowser.acknowledgeConfirm", { key: item.key == null ? t("cloudBrowser.unknownTarget") : JSON.stringify(item.key) }), t("cloudBrowser.acknowledge")) || !valid(scope)) return;
    // Invalidate polls started before this explicit local-state transition.
    statusVersion++;
    remember(await cloudObjects.recoveryAcknowledge({ transferId: item.transferId, connectionId: item.connectionId, connectionRevision: item.connectionRevision, confirmed: true }), scope);
  });
}
async function cancel(item) { await operate(async (scope) => remember(await cloudObjects.cancel(item.transferId), scope)); }
async function poll() {
  if (disposed || polling) return;
  polling = true; const scope = current(); const version = statusVersion;
  try {
    const result = await cloudObjects.status();
    if (valid(scope) && version === statusVersion) {
      transfers.value = result.filter((item) => item.connectionId === scope.connectionId);
      for (const item of transfers.value) if (item.error?.code === "CLOUD_OBJECT_PROTECTED") fail(item.error, scope);
    }
  } catch (cause) { if (version === statusVersion) fail(cause, scope); }
  finally { polling = false; }
}
watch(() => props.connectionId, async () => {
  const id = ++requestId; statusVersion++; downloadTargets.value = {}; identified.value = false; discovered.value = false; protectedRoot.value = false;
  objects.value = []; transfers.value = []; partial.value = null; prefix.value = ""; root.value = ""; next.value = null; complete.value = false; busy.value = false; loading.value = true;
  try {
    const catalog = await cloud.list();
    if (disposed || id !== requestId) return;
    connections.value = catalog.connections;
    // Object keys are relative to the backend-configured connection scope.
    prefix.value = root.value; await load();
  } catch (cause) { if (!disposed && id === requestId) { fail(cause, current()); loading.value = false; } }
}, { immediate: true });
onMounted(() => { void poll(); timer = setInterval(poll, 1500); });
onBeforeUnmount(() => { disposed = true; requestId++; statusVersion++; clearInterval(timer); });
</script>

<template>
  <section class="cloud-browser" :aria-label="t('cloudBrowser.title')">
    <header class="cloud-toolbar">
      <h2>{{ t('cloudBrowser.title') }}</h2>
      <el-select :model-value="connectionId" :aria-label="t('cloud.connection')" @update:model-value="(value) => writeRoute({ view: 'cloud', connectionId: value })"><el-option v-for="item in connections" :key="item.id" :value="String(item.id)" :label="item.name" /></el-select>
      <el-button @click="writeRoute({ view: 'settings', settingsTab: 'cloud' })">{{ t('cloudBrowser.syncSettings') }}</el-button>
    </header>
    <p>{{ t('cloudBrowser.rawOnly') }}</p>
    <div class="cloud-toolbar"><el-tag>{{ protectedRoot ? t('cloudBrowser.protected') : t('cloudBrowser.unknownReadonly') }}</el-tag><span v-if="canMutate">{{ t('cloudBrowser.attemptOnly') }}</span></div>
    <div class="cloud-toolbar">
      <el-select v-model="association" :aria-label="t('cloudBrowser.identity')"><el-option value="new" :label="t('cloudBrowser.newIdentity')" /><el-option value="existing" :label="t('cloudBrowser.existingIdentity')" /></el-select>
      <el-input v-if="association === 'existing'" v-model="physicalId" :placeholder="t('cloudBrowser.physicalId')" :aria-label="t('cloudBrowser.physicalId')" />
      <el-button data-test="prepare" :disabled="busy || loading || protectedRoot || (association === 'existing' && !physicalId.trim())" @click="prepare">{{ t('cloudBrowser.prepare') }}</el-button>
    </div>
    <nav class="cloud-toolbar" :aria-label="t('cloudBrowser.directory')"><el-button data-test="up" :disabled="prefix === root" @click="up">{{ t('cloudBrowser.up') }}</el-button><code>{{ prefix || '/' }}</code><el-button data-test="refresh" :disabled="busy || loading" @click="navigate(prefix)">{{ t('cloudBrowser.refresh') }}</el-button></nav>
    <div v-if="error" role="alert"><strong>{{ error.code }}</strong> {{ error.message }}<p v-if="error.code === 'CLOUD_OBJECT_STOP_TIMEOUT'">{{ t('cloudBrowser.stopTimeout') }}</p></div>
    <p v-if="loading" role="status">{{ t('cloudBrowser.loading') }}</p>
    <p v-if="!complete" data-test="incomplete">{{ t('cloudBrowser.incomplete') }}</p>
    <p v-if="!loading && complete && !children.length">{{ t('cloudBrowser.empty') }}</p>
    <ul class="cloud-tree" :aria-label="t('cloudBrowser.directory')">
      <li v-for="item in children" :key="item.key" :class="{ 'cloud-folder': item.directory }">
        <el-button v-if="item.directory" class="folder-link" @click="navigate(item.key)">{{ item.name }}</el-button>
        <span v-else>{{ item.name }} · {{ t('cloudBrowser.bytes', { size: item.size }) }}</span>
        <span v-if="item.protected">{{ t('cloudBrowser.protected') }}</span>
        <div class="cloud-actions"><el-button v-if="!item.directory" :disabled="busy" @click="download(item)">{{ t('cloudBrowser.download') }}</el-button><el-button :disabled="!canMutate || targetProtected(item.key)" @click="remove(item.key)">{{ t('cloudBrowser.delete') }}</el-button></div>
      </li>
    </ul>
    <el-button v-if="next" data-test="more" :disabled="loading" @click="load(true)">{{ t('cloudBrowser.more') }}</el-button>
    <div class="cloud-toolbar"><el-input v-model="uploadName" :aria-label="t('cloudBrowser.uploadName')" :placeholder="t('cloudBrowser.uploadName')" /><el-button data-test="upload" :disabled="!canMutate || objects.some((item) => item.protected && item.key === prefix + uploadName)" @click="upload">{{ t('cloudBrowser.upload') }}</el-button><el-button data-test="delete-prefix" :disabled="!canMutate || hasProtectedObjects" @click="remove(prefix)">{{ t('cloudBrowser.deletePrefix') }}</el-button></div>
    <div v-if="partial" role="status"><p>{{ t('cloudBrowser.deleteResult', { deleted: partial.deleted.length, failed: partial.failed.length }) }}</p><ul><li v-for="item in partial.failed" :key="item.key">{{ item.key }} · {{ item.error.code }} · {{ item.error.message }}</li></ul></div>
    <h3>{{ t('cloudBrowser.transfers') }}</h3>
    <ul class="cloud-tree"><li v-for="item in transfers" :key="item.transferId"><span>{{ item.transferId }} · {{ item.key == null ? t('cloudBrowser.unknownTarget') : JSON.stringify(item.key) }}<span v-if="item.sizeHint != null"> · {{ t('cloudBrowser.bytes', { size: item.sizeHint }) }}</span> · {{ t(`cloudBrowser.${item.direction}`) }} · {{ t(`cloudBrowser.phase.${item.phase}`) }}<strong v-if="item.recoveryRequired || item.phase === 'unknown'"> · {{ t('cloudBrowser.recovery') }}</strong><span v-if="item.error"> · {{ item.error.code }} · {{ item.error.message }}</span></span><el-button v-if="item.phase === 'failed' && item.direction === 'download' && item.error?.code === 'CLOUD_PRECONDITION' && downloadTargets[item.transferId]" :disabled="busy" @click="retryDownload(downloadTargets[item.transferId])">{{ t('cloudBrowser.retryDownload') }}</el-button><p v-if="item.phase === 'acknowledged'">{{ t('cloudBrowser.acknowledgedNotice') }}</p><el-button v-if="item.phase === 'unknown'" :disabled="busy" @click="acknowledge(item)">{{ t('cloudBrowser.acknowledge') }}</el-button><el-button v-if="item.phase === 'running'" :disabled="busy" @click="cancel(item)">{{ t('cloudBrowser.cancel') }}</el-button></li></ul>
  </section>
</template>

<style scoped>
.cloud-browser { flex: 1; min-width: 0; min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 12px; color: var(--app-text); padding: 16px; box-sizing: border-box; }
.cloud-browser p, .cloud-browser h2, .cloud-browser h3 { margin: 0; }
.cloud-browser p { color: var(--app-text-muted); line-height: 1.6; }
.cloud-toolbar, .cloud-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; min-width: 0; }
.cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 240px; max-width: 100%; min-width: 0; }
.cloud-tree { margin: 0; padding: 0; list-style: none; }
.cloud-tree li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px; border-bottom: 1px solid var(--app-border); }
.cloud-folder { border-left: 2px solid var(--el-color-primary); }
.cloud-browser span, .cloud-browser code, .cloud-browser [role=alert], .cloud-browser li { min-width: 0; overflow-wrap: anywhere; white-space: normal; }
.cloud-browser :deep(.el-button + .el-button) { margin-left: 0; }
.cloud-browser [role=alert] { color: var(--el-color-danger); }
@media (max-width: 480px) { .cloud-browser { padding: 10px; } .cloud-tree li { flex-wrap: wrap; align-items: flex-start; } .cloud-toolbar :deep(.el-input), .cloud-toolbar :deep(.el-select) { width: 100%; } .cloud-actions { width: 100%; } }
</style>
