import { invoke } from "./invoke.js";

const SAVE_FIELDS = ["id", "name", "endpoint", "bucket", "buckets", "region", "prefix", "pathStyle", "accessKey", "secretKey", "sessionToken", "allowInsecureHttp", "proxy", "schedule", "clearCredentials", "clearSessionToken", "clearProxyAuth"];
const MAPPING_FIELDS = ["connectionId", "repositoryId", "localRoot", "logicalPath", "schedule"];

export function toSaveS3Input(value = {}) {
  const input = {};
  for (const key of SAVE_FIELDS) {
    if (key === "id") input.id = value.id || null;
    else if (value[key] !== undefined) input[key] = value[key];
  }
  return input;
}

export function toSaveMappingInput(value = {}) {
  const input = {};
  for (const key of MAPPING_FIELDS) if (value[key] !== undefined) input[key] = value[key];
  return input;
}

export const cloud = Object.freeze({
  list: () => invoke("cloud_list"),
  probeBuckets: ({ endpoint, region = "", accessKey, secretKey, sessionToken = null, allowInsecureHttp = false, proxy = { mode: "direct" } }) => invoke("cloud_probe_buckets", { input: { endpoint, region, accessKey, secretKey, sessionToken, allowInsecureHttp, proxy } }),
  connectionCredentials: (connectionId) => invoke("cloud_connection_credentials", { connectionId }),
  saveConnection: (input) => invoke("cloud_save_connection", { input: toSaveS3Input(input) }),
  testConnection: (connectionId) => invoke("cloud_test_connection", { connectionId }),
  deleteConnection: (connectionId) => invoke("cloud_delete_connection", { connectionId }),
  createRepository: (args) => invoke("cloud_create_repository", args),
  joinRepository: (args) => invoke("cloud_join_repository", args),
  saveMapping: (mappingId, input) => invoke("cloud_save_mapping", { mappingId, input: toSaveMappingInput(input) }),
  appendDiscover: (connectionId) => invoke("cloud_append_discover", { connectionId }),
  createSyncDrive: ({ connectionId, mode, password, passwordConfirmation }) => invoke("cloud_create_sync_drive", { input: { connectionId, mode, password, passwordConfirmation } }),
  appendCreate: ({ connectionId, mode, password, passwordConfirmation }) => invoke("cloud_append_create", { input: { connectionId, mode, password, passwordConfirmation } }),
  appendJoin: ({ connectionId, anchorKey, anchorFingerprint, repositoryId, mode, acceptTofu, trustedFingerprint, password }) => invoke("cloud_append_join", { input: { connectionId, anchorKey, anchorFingerprint, repositoryId, mode, acceptTofu: acceptTofu === true, trustedFingerprint: trustedFingerprint || null, password } }),
  appendSaveMapping: (mappingId, mapping, selectedRoot) => invoke("cloud_append_save_mapping", { input: { mappingId: mappingId || null, mapping: toSaveMappingInput(mapping), selectedRoot } }),
  deleteMapping: (mappingId) => invoke("cloud_delete_mapping", { mappingId }),
  preview: (mappingId) => invoke("cloud_preview", { mappingId }),
  sync: (mappingId, previewId) => invoke("cloud_sync", { mappingId, previewId }),
  cancel: (mappingId) => invoke("cloud_cancel", { mappingId }),
  recoveryRecords: (mappingId) => invoke("cloud_recovery_records", { mappingId }),
  restore: (mappingId, recordId, destination) => invoke("cloud_restore", { mappingId, recordId, destination }),
  status: () => invoke("cloud_status"),
});

// Object browsing is independent of repository sync. Explicit payloads prevent
// native paths or unrelated fields from crossing this boundary.
export const cloudObjects = Object.freeze({
  selectBucket: (connectionId, bucket) => invoke("cloud_object_select_bucket", { connectionId, bucket }),
  list: ({ connectionId, prefix, next = null }) => invoke("cloud_object_list", { connectionId, prefix, next }),
  preview: ({ connectionId, key }) => invoke("cloud_object_preview", { connectionId, key }),
  discover: ({ connectionId, prefix }) => invoke("cloud_object_discover", { connectionId, prefix }),
  prepare: ({ connectionId }) => invoke("cloud_object_prepare", { connectionId }),
  selectLocal: ({ connectionId, direction }) => invoke("cloud_object_select_local", { connectionId, direction }),
  upload: ({ connectionId, localToken, key, overwriteEtag = null, overwriteConfirmed = false }) => invoke("cloud_object_upload", { connectionId, localToken, key, overwriteEtag, overwriteConfirmed }),
  download: ({ connectionId, localToken, key, fileName, overwriteConfirmed = false }) => invoke("cloud_object_download", { connectionId, localToken, key, fileName, overwriteConfirmed }),
  deletePlan: ({ connectionId, prefix }) => invoke("cloud_object_delete_plan", { connectionId, prefix }),
  deleteExecute: ({ connectionId, planId, confirmed }) => invoke("cloud_object_delete_execute", { connectionId, planId, confirmed }),
  recoveryAcknowledge: ({ transferId, connectionId, connectionRevision, confirmed }) => invoke("cloud_object_recovery_acknowledge", { transferId, connectionId, connectionRevision, confirmed }),
  cancel: (transferId) => invoke("cloud_object_cancel", { transferId }),
  status: () => invoke("cloud_object_status"),
});

export function objectChildren(objects, prefix) {
  const folders = new Map();
  const files = [];
  for (const object of objects) {
    if (!object.key.startsWith(prefix)) continue;
    const relative = object.key.slice(prefix.length);
    if (!relative) continue;
    const slash = relative.indexOf("/");
    if (slash >= 0) {
      const name = relative.slice(0, slash + 1);
      folders.set(name, { key: prefix + name, name, directory: true, protected: object.protected === true || folders.get(name)?.protected === true });
    } else files.push({ ...object, name: relative, directory: false });
  }
  return [...folders.values()].sort((a, b) => a.key.localeCompare(b.key)).concat(files.sort((a, b) => a.key.localeCompare(b.key)));
}

export function blankConnection() {
  return { id: "", name: "", endpoint: "", bucket: "", buckets: [], region: "", prefix: "", pathStyle: false, accessKey: "", secretKey: "", sessionToken: "", allowInsecureHttp: false, proxy: { mode: "direct" }, schedule: { enabled: false, interval: 900 }, encrypted: true, clearCredentials: false, clearSessionToken: false, clearProxyAuth: false };
}

export function blankMapping() {
  return { connectionId: "", repositoryId: "", localRoot: "", logicalPath: "", schedule: { enabled: false, interval: 900 } };
}
