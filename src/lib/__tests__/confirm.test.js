import { beforeEach, describe, expect, it, vi } from "vitest";

const { confirm } = vi.hoisted(() => ({
  confirm: vi.fn(),
}));

vi.mock("element-plus", () => ({
  ElMessageBox: { confirm },
}));

import { confirmAction } from "../confirm";
import { initLocale } from "../i18n";

describe("confirmAction", () => {
  beforeEach(() => {
    localStorage.clear();
    Object.defineProperty(globalThis.navigator, "language", {
      value: "zh-CN",
      configurable: true,
    });
    Object.defineProperty(globalThis.navigator, "languages", {
      value: ["zh-CN"],
      configurable: true,
    });
    initLocale();
    confirm.mockReset();
  });

  it("returns true only after Element Plus resolves the confirmation", async () => {
    confirm.mockResolvedValue("confirm");

    await expect(confirmAction("删除？", "删除")).resolves.toBe(true);
    expect(confirm).toHaveBeenCalledWith("删除？", "删除", {
      type: "warning",
      confirmButtonText: "确定",
      cancelButtonText: "取消",
      closeOnClickModal: false,
      closeOnPressEscape: true,
      distinguishCancelAndClose: true,
    });
  });

  it("supports an explicit danger type and safe vnode content", async () => {
    confirm.mockResolvedValue('confirm');
    const { h } = await import('vue');
    const message = h('p', { style: { color: 'red' } }, '2 sync configurations');
    await expect(confirmAction(message, 'Remove', { type: 'error' })).resolves.toBe(true);
    expect(confirm).toHaveBeenCalledWith(message, 'Remove', expect.objectContaining({ type: 'error' }));
    expect(confirm.mock.calls[0][2].dangerouslyUseHTMLString).toBeUndefined();
  });

  it("returns false when the user cancels", async () => {
    confirm.mockRejectedValue("cancel");

    await expect(confirmAction("删除？")).resolves.toBe(false);
  });
});
