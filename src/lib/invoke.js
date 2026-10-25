import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import { localizeMessage } from "./i18n.js";

/**
 * Stable code -> UI strategy mapping. Components must branch on `code`,
 * never on the localized `message` text.
 */
const ERROR_STRATEGIES = Object.freeze({
  VAULT_NOT_INITIALIZED: "initialize",
  VAULT_ALREADY_INITIALIZED: "refresh",
  VAULT_LOCKED: "unlock",
  VAULT_INVALID_MASTER_PASSWORD: "retry",
  VAULT_DATA_CORRUPTED: "support",
  VAULT_SECRET_NOT_FOUND: "support",
  VAULT_RESERVED_SECRET_NAME: "support",
  VAULT_INVALID_SECRET_NAME: "support",
  AUTH_INVALID_CREDENTIALS: "retry",
  AUTH_PASSWORD_UNCHANGED: "edit",
  AUTH_PASSWORD_CHANGE_REQUIRED: "change-admin",
  AUTH_HASH_FAILED: "support",
  VALIDATION_WEAK_PASSWORD: "edit",
  DB_OPEN_FAILED: "support",
  DB_QUERY_FAILED: "retry",
  CRYPTO_FAILED: "support",
  SERIALIZATION_FAILED: "support",
  IO_FAILED: "support",
  SHARE_NOT_FOUND: "server-refresh",
  SHARE_INVALID_CONFIG: "edit",
  POLICY_INVALID_RULE: "edit",
  SERVER_PENDING_REQUEST_NOT_FOUND: "server-refresh",
  SERVER_AUTHORIZATION_NOT_FOUND: "server-refresh",
  SERVER_ALREADY_RUNNING: "server-refresh",
  SERVER_BIND_FAILED: "edit",
  SERVER_TASK_FAILED: "support",
  PEER_INVALID_URL: "edit",
  PEER_AUTHORIZATION_REQUIRED: "show",
  PEER_AUTHORIZATION_REJECTED: "show",
  PEER_REQUEST_FAILED: "show",
  PEER_REMOTE_NOT_FOUND: "show",
  PEER_NOT_FOUND: "peer-refresh",
  PEER_PROTOCOL_ERROR: "support",
  PEER_INVALID_PATH: "edit",
  TRANSFER_TOO_LARGE: "show",
  TRANSFER_CONFLICT: "show",
  PEER_EDIT_NOT_FOUND: "show",
  PEER_OPEN_FAILED: "show",
  DISCOVERY_FAILED: "show",
  INTERNAL_ERROR: "support",
});

/**
 * Single invoke wrapper. All command calls must go through this function.
 */
export async function invoke(command, args = {}) {
  try {
    return await tauriInvoke(command, args);
  } catch (raw) {
    const error = normalizeAppError(raw);
    console.error(`[peerdav:${error.code}] ${error.message}`, {
      detail: error.detail,
      context: error.context,
    });
    throw error;
  }
}

export function normalizeAppError(raw) {
  if (
    raw &&
    typeof raw === "object" &&
    typeof raw.code === "string" &&
    typeof raw.message === "string"
  ) {
    return {
      code: raw.code,
      message: raw.message,
      detail: raw.detail ?? null,
      context: raw.context ?? null,
    };
  }

  return {
    code: "UNKNOWN_ERROR",
    message: "未知错误 / Unknown error",
    detail: typeof raw === "string" ? raw : JSON.stringify(raw),
    context: null,
  };
}

export function errorStrategy(error) {
  return ERROR_STRATEGIES[normalizeAppError(error).code] ?? "show";
}

/**
 * Returns the user-facing message in the active UI language. Backend messages
 * are bilingual; `detail` stays raw for logs and the details panel.
 */
export function toUserMessage(error) {
  return localizeMessage(normalizeAppError(error).message);
}