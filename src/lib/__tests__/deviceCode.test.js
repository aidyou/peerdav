import { describe, expect, it } from "vitest";

import { pendingIdentity, shortMachineCode } from "../deviceCode.js";

const CODE = "a1b2c3d4e5f60718293a4b5c6d7e8f90";

describe("shortMachineCode", () => {
  it("keeps the head and tail so devices stay distinguishable", () => {
    expect(shortMachineCode(CODE)).toBe("a1b2c3d4…8f90");
  });

  it("returns short values unchanged", () => {
    expect(shortMachineCode("abcd")).toBe("abcd");
    expect(shortMachineCode(CODE.slice(0, 16))).toBe(CODE.slice(0, 16));
  });

  it("returns an empty string for missing codes", () => {
    for (const value of [null, undefined, "", "   "]) {
      expect(shortMachineCode(value)).toBe("");
    }
  });
});

describe("pendingIdentity", () => {
  it("treats a reported machine code as the identity", () => {
    expect(pendingIdentity({ machineId: CODE })).toEqual({
      short: "a1b2c3d4…8f90",
      trusted: true,
    });
  });

  it("flags requests without a machine code as address-only", () => {
    for (const machineId of [null, undefined, ""]) {
      expect(pendingIdentity({ machineId })).toEqual({ short: "", trusted: false });
    }
    expect(pendingIdentity(null)).toEqual({ short: "", trusted: false });
  });
});