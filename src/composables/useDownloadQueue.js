import { computed, ref } from "vue";
import { invoke, errorStrategy } from "../lib/invoke";
import { t } from "../lib/i18n";

// Native queue IDs are capabilities; neither roots nor recursive plans enter the UI.
export function useDownloadQueue({ captureScope, scopeValid, backend, select = (scope, selection) => invoke("transfer_select_download", { backend: backend(scope), selection }), onFailure, onSelected = () => {} }) {
  const downloadDialogOpen = ref(false);
  const downloadState = ref(null);
  const downloadPage = ref({ items: [], nextOffset: null });
  const downloadSelecting = ref(false);
  const downloadCanceling = ref(false);
  const downloadResolving = ref(false);
  const downloadApplyRemaining = ref(false);
  const downloadPageLoading = ref(false);
  let session = null;
  const report = (error) => { if (errorStrategy(error) !== "ignore") onFailure(error); };
  const valid = (s) => session === s && !s.closed && scopeValid(s.scope);
  const args = (s) => ({ queueId: s.queueId });
  const downloadRunning = computed(() => downloadSelecting.value || downloadCanceling.value || ["selected", "running", "conflict", "cancelling", "canceling"].includes(downloadState.value?.phase));
  const downloadSummary = computed(() => t("peer.downloadSummary", { finished: (downloadState.value?.completedFiles || 0) + (downloadState.value?.skippedFiles || 0), total: downloadState.value?.totalFiles || 0 }));
  const downloadQueue = computed(() => downloadPage.value.items.map((item) => ({ ...item, transferId: item.id, status: ({ completed: "done", running: "downloading", active: "downloading", failed: "error", skipped: "skipped" })[item.phase] || item.phase })));
  function apply(s, state) {
    if (!valid(s)) return;
    if (state.conflict?.conflictId === s.resolvedConflictId) state = { ...state, conflict: null };
    if (downloadState.value?.conflict?.conflictId !== state.conflict?.conflictId) downloadApplyRemaining.value = false;
    downloadState.value = state;
  }
  async function release(s) {
    if (s.queueId && !s.release) s.release = invoke("transfer_release_download", args(s)).catch(report);
    await s.release;
  }
  async function disposeDownload() {
    const s = session;
    if (!s) return;
    s.closed = true; session = null;
    downloadDialogOpen.value = false; downloadState.value = null; downloadPage.value = { items: [], nextOffset: null };
    downloadSelecting.value = false; downloadCanceling.value = false; downloadResolving.value = false; downloadPageLoading.value = false;
    if (s.queueId) { await invoke("transfer_cancel_download", args(s)).catch(report); await release(s); }
  }
  async function loadDownloadPage(offset = 0) {
    const s = session;
    if (!s?.queueId || !valid(s) || downloadPageLoading.value) return;
    downloadPageLoading.value = true;
    try {
      const page = await invoke("transfer_page_download", { ...args(s), offset, limit: 100 });
      if (valid(s)) { downloadPage.value = page; s.offset = offset; }
    } catch (e) { if (valid(s)) report(e); }
    finally { if (valid(s)) downloadPageLoading.value = false; }
  }
  async function pollDownload() {
    const s = session;
    if (!s?.queueId || !valid(s) || s.starting || s.polling || downloadCanceling.value || downloadResolving.value) return;
    s.polling = true;
    const epoch = s.epoch;
    try {
      const state = await invoke("transfer_status_download", args(s));
      if (epoch === s.epoch) { apply(s, state); await loadDownloadPage(s.offset || 0); }
    }
    catch (e) { if (valid(s)) report(e); }
    finally { s.polling = false; }
  }
  async function selectDownload(selection) {
    if (downloadRunning.value) return;
    downloadSelecting.value = true;
    if (session) {
      const previous = session; previous.closed = true; session = null;
      await release(previous);
    }
    const s = { scope: captureScope(), queueId: null, starting: true, epoch: 0 };
    session = s; downloadSelecting.value = true; downloadApplyRemaining.value = false;
    downloadState.value = null; downloadPage.value = { items: [], nextOffset: null };
    try {
      const state = await select(s.scope, selection);
      if (!state) return;
      s.queueId = state.queueId;
      if (!valid(s)) { await release(s); return; }
      apply(s, state); downloadDialogOpen.value = true; onSelected();
      await loadDownloadPage();
      if (!valid(s)) return;
      const epoch = s.epoch;
      const started = await invoke("transfer_start_download", args(s));
      if (epoch === s.epoch) apply(s, started);
    } catch (e) { if (valid(s)) { report(e); if (downloadState.value) downloadState.value = { ...downloadState.value, phase: errorStrategy(e) === 'ignore' ? 'canceled' : 'failed', error: errorStrategy(e) === 'ignore' ? null : e }; } }
    finally { s.starting = false; if (valid(s)) downloadSelecting.value = false; }
  }
  async function cancelDownloadQueue() {
    const s = session;
    if (!s?.queueId || downloadCanceling.value) return;
    downloadCanceling.value = true; s.epoch++;
    try { await invoke("transfer_cancel_download", args(s)); apply(s, await invoke("transfer_status_download", args(s))); }
    catch (e) { if (valid(s)) report(e); }
    finally { if (valid(s)) downloadCanceling.value = false; }
  }
  async function resolveDownloadConflict(decision) {
    const s = session; const conflict = downloadState.value?.conflict;
    if (!s || !conflict || downloadResolving.value || downloadCanceling.value || s.resolvedConflictId === conflict.conflictId || !["overwrite", "skip"].includes(decision)) return;
    downloadResolving.value = true; s.epoch++;
    try {
      await invoke("transfer_resolve_download", { ...args(s), conflictId: conflict.conflictId, decision, applyToRemaining: downloadApplyRemaining.value });
      s.resolvedConflictId = conflict.conflictId;
      if (valid(s)) apply(s, { ...downloadState.value, conflict: null });
      apply(s, await invoke("transfer_status_download", args(s)));
    } catch (e) { if (valid(s)) report(e); }
    finally { if (valid(s)) downloadResolving.value = false; }
  }
  async function closeDownloadQueue() {
    if (downloadRunning.value) return;
    const s = session;
    if (s) { s.closed = true; session = null; await release(s); }
    downloadDialogOpen.value = false;
  }
  async function revealDownloadRoot() {
    const s = session;
    if (!s?.queueId || downloadRunning.value || !valid(s)) return;
    try { await invoke("transfer_reveal_download", args(s)); } catch (e) { if (valid(s)) report(e); }
  }
  return { downloadDialogOpen, downloadState, downloadQueue, downloadPage, downloadRunning, downloadCanceling,
    downloadResolving, downloadApplyRemaining, downloadPageLoading, downloadSummary, selectDownload, pollDownload,
    loadDownloadPage, cancelDownloadQueue, resolveDownloadConflict, closeDownloadQueue, revealDownloadRoot, disposeDownload };
}
