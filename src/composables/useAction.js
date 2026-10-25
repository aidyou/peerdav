import { ref } from "vue";

import { useAppContext } from "../lib/appContext";

/**
 * Runs one backend action with shared busy/error handling. Each view owns its
 * own busy flag so unrelated panels do not block each other.
 */
export function useAction() {
  const app = useAppContext();
  const busy = ref(false);

  async function run(action) {
    if (busy.value) return;
    busy.value = true;
    app.clearMessages();
    try {
      await action();
    } catch (error) {
      app.handleFailure(error);
    } finally {
      busy.value = false;
    }
  }

  return { busy, run };
}