import { cloudObjects } from "./cloud";
import { peerUploadBatch } from "./peer";

export const cloudUploadAdapter = {
  identity: ({ connectionId }) => ({ connectionId }),
  select: ({ connectionId, prefix }) => cloudObjects.selectUploadDirectory({ connectionId, prefix }),
  start: (args) => cloudObjects.startUploadDirectory(args),
  status: (args) => cloudObjects.statusUploadDirectory(args),
  page: (args) => cloudObjects.pageUploadDirectory(args),
  resolve: (args) => cloudObjects.resolveUploadDirectoryConflict(args),
  cancel: (args) => cloudObjects.cancelUploadDirectory(args),
  release: (args) => cloudObjects.releaseUploadDirectory(args),
  overwriteAllowed: (conflict) => Boolean(conflict.etag),
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
  overwriteAllowed: () => true,
  isCursorReset: (error) => error.code === "PEER_DIRECTORY_CURSOR_RESET" || error.code === "CLOUD_DIRECTORY_CURSOR_RESET",
  isCancellation: (error) => error?.code === "TRANSFER_CANCELED",
};
