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
  protocols: () => invoke("cloud_protocols"),
  beginOfficialAuthorization: ({ provider, clientId, redirectUri }) => invoke("cloud_official_authorize", { provider, clientId, redirectUri }),
  beginLoopbackAuthorization: ({ provider, clientId, redirectUri }) => invoke("cloud_official_begin_loopback", { provider, clientId, redirectUri }),
  waitLoopbackAuthorization: ({ requestId, name, clientSecret = null, rootId, direct = false }) => invoke("cloud_official_wait_loopback", { requestId, name, clientSecret, rootId, direct }),
  cancelOfficialAuthorization: (requestId) => invoke("cloud_official_cancel_authorization", { requestId }),
  completeOfficialAuthorization: ({ requestId, callback, name, clientSecret = null, rootId, direct = false }) => invoke("cloud_official_complete_authorization", { requestId, callback, name, clientSecret, rootId, direct }),
  driveProxy: (connectionId) => invoke('drive_proxy_settings', { connectionId }),
  updateOfficialConnection: ({ id, revision, name, clientSecret = null, clearClientSecret = false, direct = false }) => invoke('cloud_official_update_connection', { input: { id, revision, name, clientSecret, clearClientSecret, direct } }),
  list: () => invoke("cloud_list"),
  probeBuckets: ({ endpoint, region = "", accessKey, secretKey, sessionToken = null, allowInsecureHttp = false, proxy = { mode: "client" } }) => invoke("cloud_probe_buckets", { input: { endpoint, region, accessKey, secretKey, sessionToken, allowInsecureHttp, proxy } }),
  connectionCredentials: (connectionId) => invoke("cloud_connection_credentials", { connectionId }),
  saveConnection: (input) => invoke("cloud_save_connection", { input: toSaveS3Input(input) }),
  testConnection: (connectionId) => invoke("cloud_test_connection", { connectionId }),
  deleteConnection: (connectionId) => invoke("cloud_delete_connection", { connectionId }),
  createRepository: (args) => invoke("cloud_create_repository", args),
  joinRepository: (args) => invoke("cloud_join_repository", args),
  saveMapping: (mappingId, input) => invoke("cloud_save_mapping", { mappingId, input: toSaveMappingInput(input) }),
  appendDiscover: (connectionId, bucket) => invoke("cloud_append_discover", { connectionId, ...(bucket ? { bucket } : {}) }),
  createSyncDrive: ({ connectionId, bucket, mode, password, passwordConfirmation, localRoot = null, logicalPath = null }) => invoke("cloud_create_sync_drive", { input: { connectionId, ...(bucket ? { bucket } : {}), mode, password, passwordConfirmation, localRoot, logicalPath } }),
  appendCreate: ({ connectionId, mode, password, passwordConfirmation }) => invoke("cloud_append_create", { input: { connectionId, mode, password, passwordConfirmation } }),
  appendJoin: ({ connectionId, bucket, anchorKey, anchorFingerprint, repositoryId, mode, acceptTofu, trustedFingerprint, password }) => invoke("cloud_append_join", { input: { connectionId, ...(bucket ? { bucket } : {}), anchorKey, anchorFingerprint, repositoryId, mode, acceptTofu: acceptTofu === true, trustedFingerprint: trustedFingerprint || null, password } }),
  appendSaveMapping: (mappingId, mapping, selectedRoot) => invoke("cloud_append_save_mapping", { input: { mappingId: mappingId || null, mapping: toSaveMappingInput(mapping), selectedRoot } }),
  prepareMappingBind: (mapping, selectedRoot) => invoke("cloud_prepare_mapping_bind", { input: { mappingId: null, mapping: toSaveMappingInput(mapping), selectedRoot } }),
  confirmMappingBind: (planId) => invoke("cloud_confirm_mapping_bind", { planId, confirmed: true }),
  cancelMappingBind: (planId) => invoke("cloud_cancel_mapping_bind", { planId }),
  deleteMapping: (mappingId) => invoke("cloud_delete_mapping", { mappingId }),
  localEntries: (mappingId, path = '') => invoke("cloud_local_entries", { mappingId, path }),
  localPreview: (mappingId, path) => invoke('cloud_local_preview', { mappingId, path }),
  localOpen: (mappingId, path) => invoke('cloud_local_open', { mappingId, path }),
  localCreateDirectory: (mappingId, path) => invoke('cloud_local_create_directory', { mappingId, path }),
  localMutate: (mappingId, path, destination = null) => invoke('cloud_local_mutate', { mappingId, path, destination, confirmed: true }),
  localStatus: (mappingId) => invoke("cloud_local_status", { mappingId }),
  preview: (mappingId, selection) => invoke("cloud_preview", { mappingId, ...(selection?.length ? { selection } : {}) }),
  sync: (mappingId, previewId) => invoke("cloud_sync", { mappingId, previewId }),
  cancel: (mappingId) => invoke("cloud_cancel", { mappingId }),
  recoveryRecords: (mappingId) => invoke("cloud_recovery_records", { mappingId }),
  restore: (mappingId, recordId, destination) => invoke("cloud_restore", { mappingId, recordId, destination }),
  status: () => invoke("cloud_status"),
  mirrorFolders: ({ connectionId, bucket, prefix = "", next = null }) => invoke("mirror_folders", { connectionId, ...(bucket ? { bucket } : {}), prefix, next }),
  mirrorSaveMapping: (input) => invoke("mirror_save_mapping", { input }),
  mirrorDeleteMapping: (mappingId) => invoke("mirror_delete_mapping", { mappingId }),
  mirrorSync: (mappingId, force = false, planId = null) => invoke("mirror_sync", { mappingId, force, planId }),
  mirrorCancel: (mappingId) => invoke("mirror_cancel", { mappingId }),
});

// Object browsing is independent of repository sync. Explicit payloads prevent
// native paths or unrelated fields from crossing this boundary.
export const cloudObjects = Object.freeze({
  selectBucket: (connectionId, bucket) => invoke("cloud_object_select_bucket", { connectionId, bucket }),
  list: ({ connectionId, prefix, next = null }) => invoke("cloud_object_list", { connectionId, prefix, next }),
  preview: ({ connectionId, key }) => invoke("cloud_object_preview", { connectionId, key }),
  discover: ({ connectionId, prefix }) => invoke("cloud_object_discover", { connectionId, prefix }),
  prepare: ({ connectionId }) => invoke("cloud_object_prepare", { connectionId }),
  selectUploadDirectory: ({ connectionId, prefix, mode = "folder" }) => invoke("cloud_object_select_upload_directory", { connectionId, prefix, mode }),
  startUploadDirectory: ({ connectionId, batchId, conflictPolicy = "ask" }) => invoke("cloud_object_start_upload_directory", { connectionId, batchId, conflictPolicy }),
  statusUploadDirectory: ({ connectionId, batchId }) => invoke("cloud_object_status_upload_directory", { connectionId, batchId }),
  pageUploadDirectory: ({ connectionId, batchId, directoryToken, cursor = null, limit = 100 }) => invoke("cloud_object_page_upload_directory", { connectionId, batchId, ...(directoryToken == null ? {} : { directoryToken }), cursor, limit }),
  resolveUploadDirectoryConflict: ({ connectionId, batchId, conflictId, decision, applyToRemaining = false }) => invoke("cloud_object_resolve_upload_directory_conflict", { connectionId, batchId, conflictId, decision, applyToRemaining }),
  cancelUploadDirectory: ({ connectionId, batchId }) => invoke("cloud_object_cancel_upload_directory", { connectionId, batchId }),
  releaseUploadDirectory: ({ connectionId, batchId }) => invoke("cloud_object_release_upload_directory", { connectionId, batchId }),
  selectLocal: ({ connectionId, direction }) => invoke("cloud_object_select_local", { connectionId, direction }),
  upload: ({ connectionId, localToken, key, overwriteEtag = null, overwriteConfirmed = false }) => invoke("cloud_object_upload", { connectionId, localToken, key, overwriteEtag, overwriteConfirmed }),
  download: ({ connectionId, localToken, key, fileName, overwriteConfirmed = false }) => invoke("cloud_object_download", { connectionId, localToken, key, fileName, overwriteConfirmed }),
  downloadBatch: ({ connectionId, localToken, items, overwriteConfirmed = false }) => invoke("cloud_object_download_batch", { connectionId, localToken, items, overwriteConfirmed }),
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
  return { id: "", name: "", endpoint: "", bucket: "", buckets: [], region: "", prefix: "", pathStyle: false, accessKey: "", secretKey: "", sessionToken: "", allowInsecureHttp: false, proxy: { mode: "client" }, schedule: { enabled: false, interval: 120 }, encrypted: true, clearCredentials: false, clearSessionToken: false, clearProxyAuth: false };
}

export function blankMapping() {
  return { connectionId: "", repositoryId: "", localRoot: "", logicalPath: "", schedule: { enabled: false, interval: 120 } };
}
