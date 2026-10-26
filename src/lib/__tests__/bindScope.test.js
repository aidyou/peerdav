import { describe, expect, it } from "vitest";

import {
  BIND_LOCALHOST,
  BIND_WAN,
  buildBindOptions,
} from "../bindScope.js";

const LABELS = {
  wan: "WAN",
  lan: "LAN",
  local: "LOCAL",
  other: "IFACE",
  custom: "SAVED",
};

function values(options) {
  return options.map((option) => option.value);
}

describe("buildBindOptions", () => {
  it("puts the three scopes first and the other interfaces after them", () => {
    const options = buildBindOptions({
      lanAddresses: ["192.168.1.23", "100.99.99.99", "172.17.0.1"],
      current: BIND_WAN,
      labels: LABELS,
    });

    expect(values(options)).toEqual([
      BIND_WAN,
      "192.168.1.23",
      BIND_LOCALHOST,
      "100.99.99.99",
      "172.17.0.1",
    ]);
    expect(options.map((option) => option.scope)).toEqual([
      "WAN",
      "LAN",
      "LOCAL",
      "IFACE",
      "IFACE",
    ]);
    // 收起状态只显示范围名会丢掉真正绑定的地址，所以标签带上地址。
    expect(options[1].text).toBe("LAN 192.168.1.23");
  });

  it("defaults to the wide scope so a fresh pane binds every interface", () => {
    const options = buildBindOptions({
      lanAddresses: [],
      current: "",
      labels: LABELS,
    });

    expect(values(options)).toEqual([BIND_WAN, BIND_LOCALHOST]);
  });

  it("hides the lan choice when the machine has no usable address", () => {
    for (const lanAddresses of [[], ["   "], ["", null, undefined]]) {
      const options = buildBindOptions({
        lanAddresses,
        current: BIND_WAN,
        labels: LABELS,
      });
      expect(options.map((option) => option.scope)).toEqual(["WAN", "LOCAL"]);
    }
  });

  it("keeps a saved address that is no longer a candidate", () => {
    const options = buildBindOptions({
      lanAddresses: ["192.168.1.23"],
      current: "10.9.8.7",
      labels: LABELS,
    });

    expect(options.at(-1)).toMatchObject({
      value: "10.9.8.7",
      scope: "SAVED",
      address: "10.9.8.7",
    });
  });

  // 边界：候选里已有的地址不能重复追加，网卡地址撞上保留地址时也不生成两项。
  it("never duplicates a preset address", () => {
    for (const current of [BIND_WAN, BIND_LOCALHOST, " 192.168.1.23 "]) {
      const options = buildBindOptions({
        lanAddresses: ["192.168.1.23", "192.168.1.23", "0.0.0.0"],
        current,
        labels: LABELS,
      });
      expect(values(options)).toEqual([
        BIND_WAN,
        "192.168.1.23",
        BIND_LOCALHOST,
      ]);
    }
  });

  // 边界：机器只有 0.0.0.0/127.0.0.1 这类保留地址时不出现「局域网」。
  it("skips reserved addresses in the interface list", () => {
    for (const lanAddress of [BIND_WAN, BIND_LOCALHOST]) {
      const options = buildBindOptions({
        lanAddresses: [lanAddress],
        current: BIND_WAN,
        labels: LABELS,
      });
      expect(options.map((option) => option.scope)).toEqual(["WAN", "LOCAL"]);
    }
  });
});
