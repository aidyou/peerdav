import { cloudObjects } from "./cloud";
import { peerUploadBatch } from "./peer";

import { invoke, errorStrategy } from './invoke';

// Every browser transport exposes the same admitted-session queue lifecycle.
export const fileUploadAdapter = {
  identity: scope => ({ sessionId: scope.sessionId }),
  start: ({ sessionId, batchId }) => invoke('file_upload_action', { sessionId, batchId, action: 'start' }),
  status: ({ sessionId, batchId }) => invoke('file_upload_action', { sessionId, batchId, action: 'status' }),
  cancel: ({ sessionId, batchId }) => invoke('file_upload_action', { sessionId, batchId, action: 'cancel' }),
  release: ({ sessionId, batchId }) => invoke('file_release_upload', { sessionId, batchId }),
  page: ({ sessionId, batchId, directoryToken, cursor, limit }) => invoke('file_upload_page', { sessionId, batchId, directoryToken, cursor, limit }),
  resolve: ({ sessionId, batchId, conflictId, decision, applyToRemaining }) => invoke('file_upload_resolve', { sessionId, batchId, conflictId, decision, applyToRemaining }),
  overwriteAllowed: conflict => conflict?.overwriteAllowed === true,
  isCursorReset: error => errorStrategy(error) === 'refresh',
  isCancellation: error => errorStrategy(error) === 'ignore',
};

export const cloudUploadAdapter = {
  identity: ({ connectionId }) => ({ connectionId }),
  select: ({ connectionId, prefix }, mode) => cloudObjects.selectUploadDirectory({ connectionId, prefix, mode }),
  start: (args) => cloudObjects.startUploadDirectory(args),
  status: (args) => cloudObjects.statusUploadDirectory(args),
  page: (args) => cloudObjects.pageUploadDirectory(args),
  resolve: (args) => cloudObjects.resolveUploadDirectoryConflict(args),
  cancel: (args) => cloudObjects.cancelUploadDirectory(args),
  release: (args) => cloudObjects.releaseUploadDirectory(args),
  overwriteAllowed: (conflict) => conflict.overwriteAllowed !== false && (Boolean(conflict.etag) || conflict.unsafeOverwriteAllowed === true),
  isCursorReset: (error) => error.code === "CLOUD_DIRECTORY_CURSOR_RESET",
  isCancellation: (error) => error?.code === "CLOUD_CANCELLED",
};

export const peerUploadAdapter = {
  identity: ({ peerId }) => ({ peerId }),
  select: ({ peerId, shareName, remoteDir, options }, mode) => peerUploadBatch.select({ peerId, shareName, remoteDir, options, mode }),
  start: peerUploadBatch.start,
  status: peerUploadBatch.status,
  page: peerUploadBatch.page,
  resolve: peerUploadBatch.resolve,
  cancel: peerUploadBatch.cancel,
  release: peerUploadBatch.release,
  overwriteAllowed: (conflict) => conflict.overwriteAllowed !== false && conflict.unsafeOverwriteAllowed !== false,
  isCursorReset: (error) => error.code === "PEER_DIRECTORY_CURSOR_RESET" || error.code === "CLOUD_DIRECTORY_CURSOR_RESET",
  isCancellation: (error) => error?.code === "TRANSFER_CANCELED",
};
