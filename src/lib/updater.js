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

export async function checkForUpdates({ automatic = false, signal, enabled = () => true } = {}) {
  if (checking || updateBusy.value) return;
  checking = true;
  try {
    for (let attempt = 0; attempt < (automatic ? 3 : 1); attempt += 1) {
      if (signal?.aborted || (automatic && !enabled())) return;
      try {
        applyUpdateStatus(await (automatic
          ? invoke("check_for_updates", { automatic: true })
          : invoke("check_for_updates")));
        return;
      } catch (error) {
        if (!automatic) { updateState.phase = "failed"; throw error; }
        // The command boundary logs the original error and complete source chain.
        if (attempt === 2) return;
        await new Promise((resolve) => {
          const finish = () => {
            clearTimeout(timer);
            signal?.removeEventListener("abort", finish);
            resolve();
          };
          const timer = setTimeout(finish, 8000 * 2 ** attempt);
          signal?.addEventListener("abort", finish, { once: true });
          if (signal?.aborted) finish();
        });
      }
    }
  } finally { checking = false; }
}
