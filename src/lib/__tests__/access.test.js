import { describe, expect, it } from "vitest";

import {
  accessRowState,
  findAccessRequest,
  findSavedPeer,
  serverEndpoints,
  urlEndpoint,
} from "../access.js";

const SERVER = {
  serverId: "server-1",
  name: "Lab",
  url: "http://192.168.1.20:4918",
  hosts: ["192.168.1.20", "10.0.0.7"],
  port: 4918,
  shareCount: 2,
};

describe("urlEndpoint", () => {
  it("parses host and port with protocol defaults", () => {
    expect(urlEndpoint("http://192.168.1.20:4918")).toEqual({
      host: "192.168.1.20",
      port: "4918",
    });
    expect(urlEndpoint("http://Lab.local")).toEqual({ host: "lab.local", port: "80" });
    expect(urlEndpoint("https://192.168.1.20")).toEqual({
      host: "192.168.1.20",
      port: "443",
    });
  });

  it("returns null for unusable input", () => {
    for (const value of ["not a url", "", null, undefined]) {
      expect(urlEndpoint(value)).toBe(null);
    }
  });
});

describe("serverEndpoints", () => {
  it("covers the primary address and every advertised host", () => {
    const endpoints = serverEndpoints(SERVER);
    expect(endpoints).toContainEqual({ host: "192.168.1.20", port: "4918" });
    expect(endpoints).toContainEqual({ host: "10.0.0.7", port: "4918" });
    expect(serverEndpoints(null)).toEqual([]);
  });
});

describe("findSavedPeer", () => {
  it("matches fingerprints after address changes and rejects address reuse", () => {
    const peer = { id: "paired", fingerprint: "trusted", url: SERVER.url };
    expect(findSavedPeer([peer], { ...SERVER, serverId: "trusted", url: "https://new-host:5000" })).toBe(peer);
    expect(findSavedPeer([peer], { ...SERVER, serverId: "imposter" })).toBe(null);
    expect(findAccessRequest([{ ...peer, status: "waiting" }], { ...SERVER, serverId: "imposter" })).toBe(null);
  });
  it("prefers the exact url", () => {
    const peers = [{ id: "a", url: "http://192.168.1.20:4918" }];
    expect(findSavedPeer(peers, SERVER).id).toBe("a");
  });

  it("still matches when the primary address moved to another interface", () => {
    const peers = [{ id: "b", url: "http://10.0.0.7:4918" }];
    expect(findSavedPeer(peers, SERVER).id).toBe("b");
  });

  it("does not match other hosts or other ports", () => {
    expect(findSavedPeer([{ id: "c", url: "http://10.0.0.8:4918" }], SERVER)).toBe(
      null,
    );
    expect(findSavedPeer([{ id: "d", url: "http://10.0.0.7:5000" }], SERVER)).toBe(
      null,
    );
  });
});

describe("findAccessRequest", () => {
  it("matches a waiting request across advertised addresses", () => {
    const entries = [{ url: "http://10.0.0.7:4918", status: "waiting" }];
    expect(findAccessRequest(entries, SERVER)).toBe(entries[0]);
    expect(
      findAccessRequest([{ url: "http://10.0.0.9:4918", status: "waiting" }], SERVER),
    ).toBe(null);
  });
});

describe("accessRowState", () => {
  it("keeps an unrequested server actionable", () => {
    expect(accessRowState(null)).toMatchObject({
      status: "unrequested",
      canRequest: true,
      canCancel: false,
    });
  });

  it("turns a waiting request into a cancellable pending row", () => {
    expect(accessRowState({ status: "waiting" })).toMatchObject({
      status: "pending",
      canRequest: false,
      canCancel: true,
      messageKey: "home.addWaiting",
    });
  });

  it("allows requesting again after cancel, expiry or failure", () => {
    expect(accessRowState({ status: "canceled" })).toMatchObject({
      status: "canceled",
      canRequest: true,
      canCancel: false,
    });
    expect(accessRowState({ status: "expired" })).toMatchObject({
      status: "error",
      canRequest: true,
    });
    expect(
      accessRowState({ status: "failed", message: "连接超时 / timed out" }),
    ).toMatchObject({ status: "error", canRequest: true, message: "连接超时 / timed out" });
  });

  it("leaves an accessible server without an action", () => {
    expect(accessRowState({ status: "accessible" })).toMatchObject({
      canRequest: false,
      canCancel: false,
      messageKey: "home.addAlreadyAccessible",
    });
  });
});