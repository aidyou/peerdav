export const SETTINGS_TABS = ["general", "shares", "clients", "service", "account"];

export function normalizeSettingsTab(value) {
  return SETTINGS_TABS.includes(value) ? value : "general";
}

/// Parses the workspace part of the URL hash. Unknown or malformed hashes fall
/// back to Home so a stale link can never leave the workspace blank.
export function parseRoute(hash) {
  const raw = String(hash ?? "").replace(/^#\/?/, "");
  const [view, ...rest] = raw.split("/").filter(Boolean);

  if (view === "peer" && rest[0]) {
    let peerId = rest[0];
    try {
      peerId = decodeURIComponent(peerId);
    } catch {
      // Keep the raw id; the peer will simply resolve as unavailable.
    }
    return { view: "peer", peerId, settingsTab: "shares" };
  }

  if (view === "settings") {
    return {
      view: "settings",
      peerId: "",
      settingsTab: normalizeSettingsTab(rest[0]),
    };
  }

  return { view: "home", peerId: "", settingsTab: "shares" };
}

export function routeHash({ view, peerId, settingsTab } = {}) {
  if (view === "peer" && peerId) {
    return `#/peer/${encodeURIComponent(peerId)}`;
  }
  if (view === "settings") {
    return `#/settings/${normalizeSettingsTab(settingsTab)}`;
  }
  return "#/home";
}

export function readRoute() {
  return parseRoute(globalThis.location?.hash ?? "");
}

/// Writes the route into the URL hash. The hash survives a webview reload, so
/// the workspace can restore the same page instead of falling back to Home.
export function writeRoute(route) {
  const next = routeHash(route);
  const location = globalThis.location;
  if (!location) return;
  if (location.hash === next) return;
  // The first route after launch replaces the bare URL instead of adding a
  // history entry, so Back does not bounce between Home and an empty hash.
  if (!location.hash) {
    globalThis.history.replaceState(globalThis.history.state, "", next);
    return;
  }
  location.hash = next;
}