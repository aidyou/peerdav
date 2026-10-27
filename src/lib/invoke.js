import { invoke as tauriInvoke } from "@tauri-apps/api/core";

import { localizeMessage, t } from "./i18n.js";

/**
 * Stable code -> UI strategy mapping. Components must branch on `code`,
 * never on the localized `message` text.
 */
const ERROR_STRATEGIES = Object.freeze({
  VAULT_NOT_INITIALIZED: "initialize",
  VAULT_ALREADY_INITIALIZED: "refresh",
  VAULT_LOCKED: "unlock",
  VAULT_KEY_FILE_NOT_CONFIGURED: "show",
  VAULT_KEY_FILE_EXISTS: "show",
  VAULT_KEY_FILE_MISSING: "show",
  VAULT_KEY_FILE_INVALID: "show",
  VAULT_KEY_MISMATCH: "show",
  VAULT_INVALID_KEY_DIRECTORY: "edit",
  VAULT_MUST_BE_LOCKED: "show",
  VAULT_RECOVERY_CODE_NOT_CONFIGURED: "show",
  VAULT_INVALID_RECOVERY_CODE: "retry",
  VALIDATION_INVALID_USERNAME: "edit",
  PEER_ACCESS_CANCELED: "ignore",
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
  PEER_SELF_CONNECTION_FORBIDDEN: "show",
  PEER_INVALID_URL: "edit",
  PEER_AUTHORIZATION_REQUIRED: "show",
  PEER_AUTHORIZATION_REJECTED: "show",
  PEER_ACCESS_EXPIRED: "show",
  PEER_ACCESS_TIMEOUT: "show",
  PEER_ACCESS_DENIED: "show",
  PEER_REQUEST_FAILED: "show",
  PEER_IDENTITY_MISMATCH: "show",
  PEER_REMOTE_NOT_FOUND: "show",
  PEER_NOT_FOUND: "peer-refresh",
  PEER_PROTOCOL_ERROR: "support",
  PEER_INVALID_PATH: "edit",
  TRANSFER_CANCELED: "ignore",
  TRANSFER_TOO_LARGE: "show",
  PEER_PREVIEW_TOO_LARGE: "show",
  PEER_PREVIEW_TOO_MANY_LINES: "show",
  TRANSFER_CONFLICT: "show",
  UPLOAD_QUEUE_TOO_LARGE: "show",
  DOWNLOAD_QUEUE_TOO_LARGE: "show",
  PEER_EDIT_NOT_FOUND: "show",
  PEER_OPEN_FAILED: "show",
  PEER_REVEAL_FAILED: "show",
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
 * Renders error codes and safe reason parameters through the locale catalog.
 * Backend message is retained only as a compatibility fallback; detail is never
 * used as user-facing text.
 */
export function toUserMessage(error) {
  const normalized = normalizeAppError(error);
  const key = `errors.${normalized.code}`;
  let reason = normalized.context?.reason;
  if (!reason) {
    const fallback = localizeMessage(normalized.message);
    reason = fallback.split(/：|: /).slice(1).join(": ") || fallback;
  }
  if (["VALIDATION_WEAK_PASSWORD", "VALIDATION_INVALID_USERNAME"].includes(normalized.code)) {
    const reasonKey = `errors.reason.${normalized.code}.${reason}`;
    const localized = t(reasonKey);
    if (localized !== reasonKey) reason = localized;
  }
  const params = { ...normalized.context, reason };
  if (normalized.context?.status) params.reason = `HTTP ${normalized.context.status} (${reason})`;
  const translated = t(key, params);
  if (translated !== key) return translated;
  return localizeMessage(normalized.message);
}