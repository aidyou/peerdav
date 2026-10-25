import { inject } from "vue";

/**
 * Shared application state and actions provided by App.vue. Child views use
 * this instead of threading a long prop chain through the component tree.
 */
export const APP_CONTEXT = Symbol("peerdav:app-context");

export function useAppContext() {
  const context = inject(APP_CONTEXT);
  if (!context) {
    throw new Error("PeerDav app context is unavailable");
  }
  return context;
}