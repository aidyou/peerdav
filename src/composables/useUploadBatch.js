import { computed, ref } from "vue";
import { errorStrategy, normalizeAppError } from "../lib/invoke";
import { t } from "../lib/i18n";

// Both backends own the traversal. The client retains only one direct-child page.
export function useUploadBatch({ adapter, captureScope, scopeValid, canSelect, onCompleted, onFailure }) {
  const uploadDialogOpen = ref(false);
  const uploadSummary = ref(null);
  const uploadPage = ref(null);
  const uploadSelecting = ref(false);
  const uploadPageLoading = ref(false);
  const uploadBatchError = ref("");
  const uploadPageNotice = ref("");
  const uploadApplyRemaining = ref(false);
  const uploadResolving = ref(false);
  let uploadSession = null;
  const uploadRunning = computed(() => uploadSelecting.value || ["selected", "running", "conflict"].includes(uploadSummary.value?.phase));
  const uploadOverwriteAllowed = computed(() => Boolean(uploadSummary.value?.conflict && adapter.overwriteAllowed(uploadSummary.value.conflict)));
  function reportUploadFailure(cause) {
    if (errorStrategy(cause) !== "ignore") onFailure(cause);
  }
  function uploadSessionValid(session) {
    return !session.canceled && uploadSession === session && scopeValid(session.scope);
  }
  function uploadArgs(session) { return { ...adapter.identity(session.scope), batchId: session.batchId }; }
  function releaseUploadSession(session) {
    if (!session.batchId) return Promise.resolve();
    if (!session.release) session.release = adapter.release(uploadArgs(session)).catch(reportUploadFailure);
    return session.release;
  }
  async function cancelUploadSession(session = uploadSession) {
    if (!session) return;
    session.canceled = true;
    if (uploadSession === session) {
      uploadSession = null; uploadSelecting.value = false; uploadDialogOpen.value = false;
      uploadSummary.value = null; uploadPage.value = null; uploadPageLoading.value = false;
    }
    if (session.batchId) {
      await adapter.cancel(uploadArgs(session)).catch(reportUploadFailure);
      await releaseUploadSession(session);
    }
  }
  function applyUploadSummary(session, summary) {
    if (!uploadSessionValid(session)) return;
    const previousConflict = uploadSummary.value?.conflict;
    uploadSummary.value = summary;
    if (previousConflict?.conflictId !== summary.conflict?.conflictId) uploadApplyRemaining.value = false;
    // Terminal failures already travel through the backend's error event once.
    uploadBatchError.value = adapter.isCancellation(summary.error) ? "" : summary.error?.message || "";
    if (summary.phase === "completed" && !session.notified) {
      session.notified = true;
      onCompleted(summary, session.scope);
    }
  }
  async function loadUploadPage(directoryToken, cursor = null, resetting = false) {
    const session = uploadSession;
    if (!session || !uploadSessionValid(session) || uploadPageLoading.value) return;
    uploadPageLoading.value = true;
    uploadPage.value = null;
    try {
      const page = await adapter.page({ ...uploadArgs(session), directoryToken, cursor, limit: 100 });
      if (uploadSessionValid(session)) uploadPage.value = page;
    } catch (cause) {
      if (!uploadSessionValid(session)) return;
      if (adapter.isCursorReset(normalizeAppError(cause)) && !resetting) {
        uploadPageNotice.value = t("cloudBrowser.uploadCursorReset");
        uploadPageLoading.value = false;
        await loadUploadPage(undefined, null, true);
      } else if (errorStrategy(cause) !== "ignore") {
        uploadPageNotice.value = normalizeAppError(cause).message;
        reportUploadFailure(cause);
      }
    } finally { if (uploadSessionValid(session)) uploadPageLoading.value = false; }
  }
  async function pollUpload(session = uploadSession) {
    if (!session || !session.batchId || session.starting || session.stopping || session.polling || !uploadSessionValid(session)) return;
    session.polling = true;
    try {
      const epoch = session.statusEpoch;
      session.statusRequest = adapter.status(uploadArgs(session));
      const summary = await session.statusRequest;
      if (epoch === session.statusEpoch && !session.stopping) applyUploadSummary(session, summary);
    } catch (cause) {
      if (uploadSessionValid(session) && errorStrategy(cause) !== "ignore") {
        uploadBatchError.value = normalizeAppError(cause).message; reportUploadFailure(cause);
      }
    } finally { session.polling = false; }
  }
  async function cancelUpload() {
    const session = uploadSession;
    if (!session?.batchId || session.stopping) return;
    session.stopping = true;
    session.cancelRequested = true;
    try {
      await adapter.cancel(uploadArgs(session));
      session.statusEpoch++;
      if (session.statusRequest) await session.statusRequest.catch(() => null);
      if (uploadSessionValid(session)) applyUploadSummary(session, await adapter.status(uploadArgs(session)));
    } catch (cause) { if (uploadSessionValid(session)) reportUploadFailure(cause); }
    finally { session.stopping = false; }
  }
  async function closeUpload() {
    if (uploadRunning.value) return;
    const session = uploadSession;
    if (session) await cancelUploadSession(session);
    else uploadDialogOpen.value = false;
  }
  async function resolveUploadConflict(decision) {
    const session = uploadSession;
    const conflict = uploadSummary.value?.conflict;
    if (!session || !conflict?.conflictId || uploadResolving.value || (decision === "overwrite" && !adapter.overwriteAllowed(conflict))) return;
    uploadResolving.value = true;
    const stillCurrent = () => uploadSessionValid(session) && uploadSummary.value?.conflict?.conflictId === conflict.conflictId;
    try {
      await adapter.resolve({ ...uploadArgs(session), conflictId: conflict.conflictId, decision, applyToRemaining: uploadApplyRemaining.value });
      if (stillCurrent()) await pollUpload(session);
    } catch (cause) { if (stillCurrent()) reportUploadFailure(cause); }
    finally { if (uploadSessionValid(session)) uploadResolving.value = false; }
  }
  async function selectUpload(mode = "folder") {
    if (!canSelect() || uploadRunning.value) return;
    if (uploadSession) await closeUpload();
    const session = { scope: captureScope(), batchId: null, canceled: false, starting: true, statusEpoch: 0 };
    uploadSession = session;
    uploadSelecting.value = true; uploadSummary.value = null; uploadPage.value = null;
    uploadBatchError.value = ""; uploadPageNotice.value = ""; uploadApplyRemaining.value = false; uploadResolving.value = false;
    try {
      const summary = await adapter.select(session.scope, mode);
      if (!summary) { if (uploadSession === session) { uploadSession = null; uploadSelecting.value = false; } return; }
      session.batchId = summary.batchId;
      if (!uploadSessionValid(session)) { await releaseUploadSession(session); return; }
      applyUploadSummary(session, summary);
      uploadDialogOpen.value = true;
      await loadUploadPage();
      if (!uploadSessionValid(session) || session.cancelRequested) return;
      const started = await adapter.start({ ...uploadArgs(session), conflictPolicy: "ask" });
      if (!uploadSessionValid(session)) {
        await adapter.cancel(uploadArgs(session)).catch(reportUploadFailure);
        await releaseUploadSession(session);
        return;
      }
      if (session.cancelRequested) {
        await adapter.cancel(uploadArgs(session));
        applyUploadSummary(session, await adapter.status(uploadArgs(session)));
      } else applyUploadSummary(session, started);
    } catch (cause) {
      if (uploadSessionValid(session) && errorStrategy(cause) === "ignore") {
        if (uploadSummary.value) uploadSummary.value = { ...uploadSummary.value, phase: "cancelled" };
      } else if (uploadSessionValid(session)) {
        uploadBatchError.value = normalizeAppError(cause).message; uploadDialogOpen.value = true;
        if (uploadSummary.value) uploadSummary.value = { ...uploadSummary.value, phase: "failed" };
        reportUploadFailure(cause);
      }
    } finally {
      session.starting = false;
      if (uploadSession === session) uploadSelecting.value = false;
    }
  }
  return { uploadDialogOpen, uploadSummary, uploadPage, uploadRunning, uploadPageLoading,
    uploadBatchError, uploadPageNotice, uploadApplyRemaining, uploadResolving, uploadOverwriteAllowed,
    selectUpload, loadUploadPage, pollUpload, cancelUpload, closeUpload, resolveUploadConflict, cancelUploadSession };
}
