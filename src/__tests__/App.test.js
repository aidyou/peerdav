import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { windowMock, eventHandlers } = vi.hoisted(() => ({
  windowMock: {
    close: vi.fn(),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
  },
  eventHandlers: new Map(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event, handler) => {
    eventHandlers.set(event, handler);
    return () => eventHandlers.delete(event);
  }),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => windowMock,
}));

vi.mock("../lib/invoke", async (importOriginal) => {
  const actual = await importOriginal();
  return { ...actual, invoke: vi.fn() };
});

import App from "../App.vue";
import { LOCALE_STORAGE_KEY, initLocale } from "../lib/i18n";
import { invoke } from "../lib/invoke";

const SERVER_STATUS = {
  running: false,
  bindAddress: null,
  discoveryRunning: false,
  discoveryPort: null,
  shareCount: 0,
  pendingCount: 0,
};

const PEER = {
  id: "peer-1",
  label: "Office",
  url: "http://192.168.1.10:4918",
  online: false,
  shares: [],
};

function mockCommands(overrides = {}) {
  const handlers = {
    vault_status: async () => ({ initialized: true, unlocked: true }),
    server_status: async () => ({ ...SERVER_STATUS }),
    list_peers: async () => [],
    admin_login: async () => ({
      username: "admin",
      mustChangePassword: false,
    }),
    list_shares: async () => [],
    list_pending_requests: async () => [],
    list_authorizations: async () => [],
    scan_lan: async () => [],
    check_peer_health: async () => ({
      online: true,
      authorized: false,
      latencyMs: null,
      code: "AUTH_INVALID_CREDENTIALS",
      message:
        "管理员登录失败：用户名或密码错误 / Admin login failed: invalid username or password.",
    }),
    ...overrides,
  };

  invoke.mockImplementation(async (command, args) => {
    const handler = handlers[command];
    if (!handler) throw new Error(`unexpected command: ${command}`);
    return handler(args);
  });
}

async function mountReady(overrides = {}) {
  mockCommands(overrides);
  const wrapper = mount(App);
  await flushPromises();
  return wrapper;
}

const VAULT_NOT_INITIALIZED = {
  vault_status: async () => ({ initialized: false, unlocked: false }),
};

async function signIn(wrapper, password = "secret") {
  await wrapper.find('input[autocomplete="username"]').setValue("admin");
  await wrapper.find('input[type="password"]').setValue(password);
  await wrapper.find("form").trigger("submit");
  await flushPromises();
}

function findButton(wrapper, label) {
  const button = wrapper.findAll("button").find((item) => item.text() === label);
  if (!button) throw new Error(`button not found: ${label}`);
  return button;
}

async function clickButton(wrapper, label) {
  await findButton(wrapper, label).trigger("click");
  await flushPromises();
}

async function switchLanguage(wrapper, locale) {
  const index = locale === "en" ? 1 : 0;
  await wrapper.findAll(".language-switch button").at(index).trigger("click");
  await flushPromises();
}

describe("App single-language rendering", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("renders only simplified Chinese by default", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    expect(wrapper.text()).toContain("首次初始化");
    expect(wrapper.text()).not.toContain("First-time setup");
    expect(wrapper.text()).not.toContain(" / ");
    expect(document.documentElement.lang).toBe("zh-CN");

    wrapper.unmount();
  });

  it("switches the whole screen to English and persists the choice", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    await switchLanguage(wrapper, "en");

    expect(wrapper.text()).toContain("First-time setup");
    expect(wrapper.text()).not.toContain("首次初始化");
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe("en");
    expect(document.documentElement.lang).toBe("en");
    expect(
      wrapper.findAll(".language-switch button").at(1).attributes("aria-pressed"),
    ).toBe("true");

    wrapper.unmount();
  });

  it("starts in the persisted language", async () => {
    localStorage.setItem(LOCALE_STORAGE_KEY, "en");
    initLocale();

    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    expect(wrapper.text()).toContain("First-time setup");
    expect(wrapper.text()).not.toContain("首次初始化");

    wrapper.unmount();
  });
});

describe("window controls", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
    windowMock.close.mockClear();
    windowMock.minimize.mockClear();
    windowMock.toggleMaximize.mockClear();
  });

  it("dispatches close, minimize and maximize to the current window", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    await wrapper.find(".traffic-light.close").trigger("click");
    await wrapper.find(".traffic-light.minimize").trigger("click");
    await wrapper.find(".traffic-light.maximize").trigger("click");
    await flushPromises();

    expect(windowMock.close).toHaveBeenCalledTimes(1);
    expect(windowMock.minimize).toHaveBeenCalledTimes(1);
    expect(windowMock.toggleMaximize).toHaveBeenCalledTimes(1);

    wrapper.unmount();
  });
});

describe("quit confirmation", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
    eventHandlers.clear();
  });

  it("asks before quitting when the tray requests an exit", async () => {
    const quit = vi.fn(async () => {});
    const wrapper = await mountReady({
      ...VAULT_NOT_INITIALIZED,
      quit_app: quit,
    });

    const requestQuit = eventHandlers.get("app://quit-request");
    expect(typeof requestQuit).toBe("function");
    requestQuit();
    await flushPromises();

    expect(wrapper.text()).toContain("确定要退出 PeerDav 吗？");
    await clickButton(wrapper, "退出");
    await flushPromises();

    expect(quit).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });
});

describe("first-time setup", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  const NOT_INITIALIZED = {
    vault_status: async () => ({ initialized: false, unlocked: false }),
  };

  async function fillSetup(wrapper, { confirm = "admin password 2026" } = {}) {
    const encryptionInputs = wrapper.find("form").findAll("input");
    await encryptionInputs[0].setValue("correct horse battery 2026");
    await encryptionInputs[1].setValue("correct horse battery 2026");
    await findButton(wrapper, "下一步").trigger("click");
    await flushPromises();

    const adminInputs = wrapper.find("form").findAll("input");
    await adminInputs[0].setValue("owner");
    await adminInputs[1].setValue("admin password 2026");
    await adminInputs[2].setValue(confirm);
  }

  it("requires a 6-character encryption password before the admin step", async () => {
    const wrapper = await mountReady(NOT_INITIALIZED);

    const inputs = wrapper.find("form").findAll("input");
    await inputs[0].setValue("short");
    await inputs[1].setValue("short");
    await findButton(wrapper, "下一步").trigger("click");
    await flushPromises();

    expect(wrapper.find(".alert-error").text()).toBe(
      "密码长度至少为 6 个字符。",
    );
    expect(wrapper.find('input[autocomplete="username"]').exists()).toBe(false);

    wrapper.unmount();
  });

  it("enforces the password composition rules before the admin step", async () => {
    const cases = [
      ["abcdef", "密码必须包含至少一个数字。"],
      ["123456", "密码必须包含至少一个英文字母。"],
      [" abc123", "密码首尾不能包含空格。"],
      ["密码abc123", "密码只能包含英文字母、数字和符号。"],
    ];

    for (const [password, message] of cases) {
      const wrapper = await mountReady(NOT_INITIALIZED);
      const inputs = wrapper.find("form").findAll("input");
      await inputs[0].setValue(password);
      await inputs[1].setValue(password);
      await findButton(wrapper, "下一步").trigger("click");
      await flushPromises();

      expect(wrapper.find(".alert-error").text()).toBe(message);
      expect(wrapper.find('input[autocomplete="username"]').exists()).toBe(
        false,
      );

      wrapper.unmount();
    }
  });

  it("creates the vault with the admin credentials entered by the user", async () => {
    const initialize = vi.fn(async (args) => ({
      adminUsername: args.adminUsername,
      vaultStatus: { initialized: true, unlocked: true },
    }));
    const wrapper = await mountReady({
      ...NOT_INITIALIZED,
      initialize_vault: initialize,
    });

    await fillSetup(wrapper);
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(initialize).toHaveBeenCalledWith({
      masterPassword: "correct horse battery 2026",
      adminUsername: "owner",
      adminPassword: "admin password 2026",
    });
    expect(
      wrapper.find('input[autocomplete="username"]').element.value,
    ).toBe("owner");
    expect(wrapper.text()).toContain("管理员登录");

    wrapper.unmount();
  });

  it("rejects mismatched admin passwords before calling the backend", async () => {
    const initialize = vi.fn();
    const wrapper = await mountReady({
      ...NOT_INITIALIZED,
      initialize_vault: initialize,
    });

    await fillSetup(wrapper, { confirm: "another password 2026" });
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(initialize).not.toHaveBeenCalled();
    expect(wrapper.find(".alert-error").text()).toBe(
      "两次输入的管理员密码不一致。",
    );

    wrapper.unmount();
  });
});

describe("error rendering", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("shows one localized half of a backend error and retranslates on switch", async () => {
    const wrapper = await mountReady({
      vault_status: async () => {
        throw {
          code: "VAULT_LOCKED",
          message:
            "保险箱已锁定，请先解锁 / The vault is locked. Unlock it first.",
          detail: "locked",
          context: { operation: "vault_status" },
        };
      },
    });

    const alert = wrapper.find(".alert-error");
    expect(alert.exists()).toBe(true);
    expect(alert.text()).toBe("保险箱已锁定，请先解锁");

    await switchLanguage(wrapper, "en");
    expect(wrapper.find(".alert-error").text()).toBe(
      "The vault is locked. Unlock it first.",
    );

    wrapper.unmount();
  });
});

describe("peer health and LAN state", () => {
  let health;

  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
    health = {
      online: true,
      authorized: false,
      latencyMs: null,
      code: "PEER_AUTHORIZATION_REJECTED",
      message: "对端拒绝了授权码 / The peer rejected the authorization code.",
    };
  });

  it("distinguishes online, authorized and offline states", async () => {
    const wrapper = await mountReady({
      list_peers: async () => [PEER],
      check_peer_health: async () => health,
    });
    await signIn(wrapper);
    expect(wrapper.text()).toContain("共享 0 · 待审批 0 · 发现 关闭");

    await clickButton(wrapper, "检测");
    expect(wrapper.text()).toContain("在线 · 未授权 · 对端拒绝了授权码");

    health = { online: true, authorized: true, latencyMs: 12, code: null, message: null };
    await clickButton(wrapper, "检测");
    expect(wrapper.text()).toContain("在线 · 已授权 · 12 ms");

    health = {
      online: false,
      authorized: false,
      latencyMs: null,
      code: "PEER_REQUEST_FAILED",
      message: "请求对端失败：连接被拒绝 / Peer request failed: connection refused.",
    };
    await clickButton(wrapper, "检测");
    expect(wrapper.text()).toContain("离线 · 请求对端失败：连接被拒绝");

    wrapper.unmount();
  });

  it("shows an explicit empty state after a LAN scan finds nothing", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);

    await clickButton(wrapper, "扫描局域网");
    expect(wrapper.text()).toContain(
      "未发现服务器，请确认在同一网段且服务已启动。",
    );

    await switchLanguage(wrapper, "en");
    expect(wrapper.text()).toContain(
      "No server found. Check that both devices are on the same LAN and the server is running.",
    );

    wrapper.unmount();
  });

  it("handles a failing peer refresh without an unhandled rejection", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    let failListPeers = false;

    const wrapper = await mountReady({
      list_peers: async () => {
        if (failListPeers) {
          throw {
            code: "PEER_NOT_FOUND",
            message: "未找到该对端 / Peer not found.",
          };
        }
        return [PEER];
      },
      check_peer_health: async () => {
        failListPeers = true;
        throw {
          code: "PEER_NOT_FOUND",
          message: "未找到该对端 / Peer not found.",
        };
      },
    });
    await signIn(wrapper);

    await clickButton(wrapper, "检测");
    await flushPromises();

    expect(
      errorSpy.mock.calls.some(([message]) =>
        String(message).includes("PEER_NOT_FOUND"),
      ),
    ).toBe(true);

    errorSpy.mockRestore();
    wrapper.unmount();
  });
});

describe("share management", () => {
  const SHARE = {
    id: "share-1",
    name: "Docs",
    rootPath: "/srv/docs",
    permissions: { canRead: true, canWrite: false, canDelete: false },
    rules: [
      { kind: "allow", matcher: { type: "network", value: "192.168.1.0/24" } },
      { kind: "deny", matcher: { type: "ip", value: "192.168.1.9" } },
    ],
  };

  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("edits existing share permissions and rules through the admin commands", async () => {
    const wrapper = await mountReady({
      list_shares: async () => [SHARE],
      update_share_permissions: async () => SHARE,
      set_share_rules: async () => SHARE,
    });
    await signIn(wrapper);

    await wrapper
      .find('input[type="password"][autocomplete="current-password"]')
      .setValue("admin-secret");
    await clickButton(wrapper, "刷新列表");
    expect(wrapper.text()).toContain("Docs");

    await clickButton(wrapper, "编辑");
    expect(wrapper.text()).toContain(
      "正在编辑“Docs”；名称和目录不可修改。",
    );

    const lockedTextInputs = wrapper
      .findAll('input[type="text"]')
      .filter((input) => input.element.disabled);
    expect(lockedTextInputs).toHaveLength(2);
    expect(lockedTextInputs[0].element.value).toBe("Docs");
    expect(lockedTextInputs[1].element.value).toBe("/srv/docs");

    const saveButton = findButton(wrapper, "保存修改");
    expect(saveButton.attributes("type")).toBe("submit");
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(invoke).toHaveBeenCalledWith(
      "update_share_permissions",
      expect.objectContaining({
        username: "admin",
        password: "admin-secret",
        shareId: "share-1",
        permissions: { canRead: true, canWrite: false, canDelete: false },
      }),
    );
    expect(invoke).toHaveBeenCalledWith(
      "set_share_rules",
      expect.objectContaining({
        shareId: "share-1",
        rules: [
          {
            kind: "allow",
            matcher: { type: "network", value: "192.168.1.0/24" },
          },
          { kind: "deny", matcher: { type: "ip", value: "192.168.1.9" } },
        ],
      }),
    );

    wrapper.unmount();
  });
});