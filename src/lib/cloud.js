import { invoke } from "./invoke.js";

const SAVE_FIELDS = ["id", "name", "endpoint", "bucket", "region", "prefix", "pathStyle", "accessKey", "secretKey", "sessionToken", "allowInsecureHttp", "proxy", "schedule", "clearCredentials", "clearSessionToken", "clearProxyAuth"];
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
  saveConnection: (input) => invoke("cloud_save_connection", { input: toSaveS3Input(input) }),
  testConnection: (connectionId) => invoke("cloud_test_connection", { connectionId }),
  deleteConnection: (connectionId) => invoke("cloud_delete_connection", { connectionId }),
  createRepository: (args) => invoke("cloud_create_repository", args),
  joinRepository: (args) => invoke("cloud_join_repository", args),
  saveMapping: (mappingId, input) => invoke("cloud_save_mapping", { mappingId, input: toSaveMappingInput(input) }),
  deleteMapping: (mappingId) => invoke("cloud_delete_mapping", { mappingId }),
  preview: (mappingId) => invoke("cloud_preview", { mappingId }),
  sync: (mappingId, previewId) => invoke("cloud_sync", { mappingId, previewId }),
  cancel: (mappingId) => invoke("cloud_cancel", { mappingId }),
  recoveryRecords: (mappingId) => invoke("cloud_recovery_records", { mappingId }),
  restore: (mappingId, recordId, destination) => invoke("cloud_restore", { mappingId, recordId, destination }),
  status: () => invoke("cloud_status"),
});

export function blankConnection() {
  return { id: "", name: "", endpoint: "", bucket: "", region: "", prefix: "", pathStyle: false, accessKey: "", secretKey: "", sessionToken: "", allowInsecureHttp: false, proxy: { mode: "direct" }, schedule: { enabled: false, interval: 900 }, encrypted: true, clearCredentials: false, clearSessionToken: false, clearProxyAuth: false };
}

export function blankMapping() {
  return { connectionId: "", repositoryId: "", localRoot: "", logicalPath: "", schedule: { enabled: false, interval: 900 } };
}
