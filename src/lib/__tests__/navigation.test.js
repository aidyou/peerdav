import { afterEach, describe, expect, it } from "vitest";

import {
  normalizeSettingsTab,
  parseRoute,
  readRoute,
  routeHash,
  writeRoute,
} from "../navigation";

afterEach(() => {
  window.history.replaceState(null, "", window.location.pathname);
});

describe("workspace routes", () => {
  it("falls back to Home for empty, unknown and malformed hashes", () => {
    for (const hash of ["", "#", "#/home", "#/unknown", "#/peer/"]) {
      expect(parseRoute(hash)).toEqual({
        view: "home",
        peerId: "",
        settingsTab: "shares",
      });
    }
  });

  it("parses and decodes a peer page", () => {
    expect(parseRoute("#/peer/peer%2F1")).toEqual({
      view: "peer",
      peerId: "peer/1",
      settingsTab: "shares",
    });
  });

  it("normalizes the settings tab", () => {
    expect(parseRoute("#/settings/clients")).toEqual({
      view: "settings",
      peerId: "",
      settingsTab: "clients",
    });
    expect(parseRoute("#/settings/unknown").settingsTab).toBe("general");
    expect(normalizeSettingsTab("account")).toBe("account");
    expect(normalizeSettingsTab("")).toBe("general");
  });

  it("builds stable hashes for each page", () => {
    expect(routeHash({ view: "home" })).toBe("#/home");
    expect(routeHash({ view: "peer", peerId: "peer/1" })).toBe(
      "#/peer/peer%2F1",
    );
    expect(routeHash({ view: "settings", settingsTab: "service" })).toBe(
      "#/settings/service",
    );
    expect(routeHash({ view: "settings" })).toBe("#/settings/general");
  });

  it("writes the route into the hash and reads it back", () => {
    writeRoute({ view: "peer", peerId: "peer 1" });
    expect(window.location.hash).toBe("#/peer/peer%201");
    expect(readRoute()).toEqual({
      view: "peer",
      peerId: "peer 1",
      settingsTab: "shares",
    });
  });
});