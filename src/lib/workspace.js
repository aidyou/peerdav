export const SETTINGS_SEEN_STORAGE_KEY = "peerdav.settings-seen";

/// The very first successful admin login lands on Settings so the user can
/// finish configuring shares and the service. Later launches start on Home.
export function shouldOpenSettingsOnLogin() {
  try {
    return localStorage.getItem(SETTINGS_SEEN_STORAGE_KEY) !== "1";
  } catch {
    return true;
  }
}

export function markSettingsSeen() {
  try {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
  } catch {
    // Storage may be unavailable; the current login still proceeds.
  }
}