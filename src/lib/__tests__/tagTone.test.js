import { describe, expect, it } from "vitest";

import { statusTagTone } from "../tagTone.js";

describe("statusTagTone", () => {
  it("maps ready states to the green tone", () => {
    for (const status of [
      "approved",
      "authorized",
      "connected",
      "online",
      "ready",
      "done",
    ]) {
      expect(statusTagTone(status)).toBe("success");
    }
  });

  it("maps failure states to the red tone", () => {
    for (const status of ["offline", "error", "failed", "denied"]) {
      expect(statusTagTone(status)).toBe("danger");
    }
  });

  it("maps states that need the user to act to the attention tone", () => {
    for (const status of ["pending", "requesting"]) {
      expect(statusTagTone(status)).toBe("warning");
    }
  });

  it("maps automatic progress and no-result states to the neutral tone", () => {
    for (const status of [
      "waiting",
      "queued",
      "downloading",
      "uploading",
      "unrequested",
      "canceled",
      "skipped",
      "unknown",
    ]) {
      expect(statusTagTone(status)).toBe("info");
    }
  });

  // 边界：新增或空的状态键落回中性灰，不能借用强调色——强调色只表示可操作。
  it("falls back to the neutral tone for empty and unknown status keys", () => {
    expect(statusTagTone("")).toBe("info");
    expect(statusTagTone("not-a-status")).toBe("info");
    expect(statusTagTone(undefined)).toBe("info");
  });
});
