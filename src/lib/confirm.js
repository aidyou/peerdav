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
  // Element Plus renders `title` as plain text only, so richer headings use
  // headerless mode: the caller renders its own heading inside the message body
  // and CSS floats the close button into the empty header area.
  const headerless = options.headerless === true;
  // Destructive confirmations set danger: true so the action button reads as
  // irreversible instead of a neutral primary "Confirm" (see src/assets/confirm.css).
  const danger = options.danger === true;
  const customClass = [headerless && "confirm-box--headerless", danger && "confirm-box--danger"].filter(Boolean).join(" ");
  try {
    await ElMessageBox.confirm(message, headerless ? "" : title, {
      // type: null suppresses the status icon for dialogs that render their own title icon.
      ...(options.type === null ? {} : { type: options.type ?? "warning" }),
      confirmButtonText: options.confirmText ?? t("confirm.ok"),
      cancelButtonText: options.cancelText ?? t("confirm.cancel"),
      closeOnClickModal: false,
      closeOnPressEscape: true,
      distinguishCancelAndClose: true,
      ...(customClass ? { customClass } : {}),
    });
    return true;
  } catch {
    return false;
  }
}
