import { computed, reactive } from "vue";
import { invoke } from "./invoke";

export const updateState = reactive({ configured: false, phase: "idle", currentVersion: "", version: null, notes: null, downloaded: 0, total: null });
export const updateBusy = computed(() => ["checking", "downloading", "installing"].includes(updateState.phase));
export const updateProgress = computed(() => updateState.total > 0
  ? Math.min(99, Math.max(0, Math.floor(updateState.downloaded / updateState.total * 100))) : 0);
let checking = false;

export function applyUpdateStatus(status) {
  if (status && typeof status.phase === "string") Object.assign(updateState, status);
}

export async function checkForUpdates() {
  if (checking || updateBusy.value) return;
  checking = true;
  try { applyUpdateStatus(await invoke("check_for_updates")); }
  catch (error) { updateState.phase = "failed"; throw error; }
  finally { checking = false; }
}
