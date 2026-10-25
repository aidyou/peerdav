import { ElMessageBox } from "element-plus";
import "element-plus/es/components/message-box/style/css";

import { t } from "./i18n";

/**
 * Shows the Element Plus confirmation dialog and waits for an explicit user
 * choice. A close, escape, or cancel all resolve to false, so callers must not
 * perform the action until this promise resolves true. The optional labels are
 * used when the second choice means "skip" rather than "cancel".
 */
export async function confirmAction(
  message,
  title = t("confirm.title"),
  options = {},
) {
  try {
    await ElMessageBox.confirm(message, title, {
      type: "warning",
      confirmButtonText: options.confirmText ?? t("confirm.ok"),
      cancelButtonText: options.cancelText ?? t("confirm.cancel"),
      closeOnClickModal: false,
      closeOnPressEscape: true,
      distinguishCancelAndClose: true,
    });
    return true;
  } catch {
    return false;
  }
}
