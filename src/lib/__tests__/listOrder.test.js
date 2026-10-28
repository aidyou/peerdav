import { describe, expect, it } from "vitest";
import { stableList } from "../listOrder";

describe("stable device and share lists", () => {
  it("keeps identity order across backend permutations and status changes", () => {
    const records = [{ id: "b", status: "pending" }, { id: "a", status: "approved" }];
    expect(stableList(records, (item) => item.id).map((item) => item.id)).toEqual(["a", "b"]);
    expect(stableList([...records].reverse().map((item) => ({ ...item, status: "approved" })), (item) => item.id).map((item) => item.id)).toEqual(["a", "b"]);
    expect(records[0].id).toBe("b");
  });
});
