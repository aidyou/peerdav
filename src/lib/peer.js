import { invoke } from "./invoke";

// Native selection returns opaque batch capabilities, never local absolute paths.
export const peerUploadBatch = Object.freeze({
  select: ({ peerId, shareName, remoteDir, mode, options }) => invoke("peer_select_upload_batch", { peerId, shareName, remoteDir, mode, options }),
  start: ({ peerId, batchId, conflictPolicy = "ask" }) => invoke("peer_start_upload_batch", { peerId, batchId, conflictPolicy }),
  status: ({ peerId, batchId }) => invoke("peer_status_upload_batch", { peerId, batchId }),
  page: ({ peerId, batchId, directoryToken, cursor = null, limit = 100 }) => invoke("peer_page_upload_batch", { peerId, batchId, ...(directoryToken == null ? {} : { directoryToken }), cursor, limit }),
  resolve: ({ peerId, batchId, conflictId, decision, applyToRemaining = false }) => invoke("peer_resolve_upload_batch", { peerId, batchId, conflictId, decision, applyToRemaining }),
  cancel: ({ peerId, batchId }) => invoke("peer_cancel_upload_batch", { peerId, batchId }),
  release: ({ peerId, batchId }) => invoke("peer_release_upload_batch", { peerId, batchId }),
});
