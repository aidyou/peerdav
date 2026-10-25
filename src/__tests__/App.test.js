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

vi.mock("@tauri-apps/plugin-autostart", () => ({
  isEnabled: vi.fn(async () => false),
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => "/tmp/peerdav-keys"),
  save: vi.fn(async () => "/tmp/peerdav-save.txt"),
}));

vi.mock("../lib/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

vi.mock("../lib/invoke", async (importOriginal) => {
  const actual = await importOriginal();
  return { ...actual, invoke: vi.fn() };
});

import App from "../App.vue";
import { LOCALE_STORAGE_KEY, initLocale } from "../lib/i18n";
import { THEME_STORAGE_KEY, initTheme } from "../lib/theme";
import { ACCENT_STORAGE_KEY, initAccent } from "../lib/accent";
import { SETTINGS_SEEN_STORAGE_KEY } from "../lib/workspace";
import { confirmAction } from "../lib/confirm";
import { invoke } from "../lib/invoke";

// Pin the host to a Chinese locale so initLocale() resolves to zh-CN and the
// rendered-text assertions stay stable.
Object.defineProperty(window.navigator, "language", {
  value: "zh-CN",
  configurable: true,
});
Object.defineProperty(window.navigator, "languages", {
  value: ["zh-CN"],
  configurable: true,
});

beforeEach(() => {
  confirmAction.mockReset();
  confirmAction.mockResolvedValue(true);
  invoke.mockReset();
});

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

const DISCOVERED = {
  serverId: "server-1",
  name: "Lab",
  url: "http://192.168.1.20:4918",
  hosts: ["192.168.1.20"],
  port: 4918,
  shareCount: 2,
};

function mockCommands(overrides = {}) {
  const handlers = {
    vault_status: async () => ({ initialized: true, unlocked: true }),
    server_status: async () => ({ ...SERVER_STATUS }),
    resume_server: async () => ({ ...SERVER_STATUS }),
    list_peers: async () => [],
    sync_peers: async () => 0,
    local_networks: async () => ["192.168.1.0/24"],
    request_peer_access: async () => ({
      requested: true,
      requestId: "request-1",
    }),
    wait_for_peer_access: async () => PEER,
    cancel_peer_access: async () => {},
    list_peer_shares: async () => [],
    admin_login: async () => ({
      username: "admin",
      mustChangePassword: false,
    }),
    admin_session: async () => null,
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
  await wrapper
    .find('[data-menu="language"] .titlebar-menu__trigger')
    .trigger("click");
  await flushPromises();
  await wrapper
    .find(`[data-menu="language"] .titlebar-menu__item[data-value="${locale}"]`)
    .trigger("click");
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
    await wrapper
      .find('[data-menu="language"] .titlebar-menu__trigger')
      .trigger("click");
    await flushPromises();
    expect(
      wrapper
        .find('[data-menu="language"] .titlebar-menu__item[data-value="en"]')
        .attributes("aria-checked"),
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
    await findButton(wrapper, "选择目录").trigger("click");
    await flushPromises();
    await findButton(wrapper, "下一步").trigger("click");
    await flushPromises();

    const adminInputs = wrapper.find("form").findAll("input");
    await adminInputs[0].setValue("owner");
    await adminInputs[1].setValue("admin password 2026");
    await adminInputs[2].setValue(confirm);
  }

  it("requires a key directory before the admin step", async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    open.mockResolvedValueOnce(null);
    const wrapper = await mountReady(NOT_INITIALIZED);

    await findButton(wrapper, "选择目录").trigger("click");
    await flushPromises();
    await findButton(wrapper, "下一步").trigger("click");
    await flushPromises();

    expect(wrapper.find(".alert-error").text()).toBe("请先选择密钥保存目录。");
    expect(wrapper.find('input[autocomplete="username"]').exists()).toBe(false);

    wrapper.unmount();
  });

  it("creates the vault with the admin credentials entered by the user", async () => {
    const initialize = vi.fn(async (args) => ({
      adminUsername: args.adminUsername,
      vaultStatus: { initialized: true, unlocked: true },
      recoveryCode: "abcde-fghij-klmno-pqrst-uvwxy-zabcd-efghi-jklmn",
    }));
    const wrapper = await mountReady({
      ...NOT_INITIALIZED,
      initialize_vault: initialize,
    });

    await fillSetup(wrapper);
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(initialize).toHaveBeenCalledWith({
      keyDirectory: "/tmp/peerdav-keys",
      adminUsername: "owner",
      adminPassword: "admin password 2026",
    });
    expect(
      wrapper.find('input[autocomplete="username"]').element.value,
    ).toBe("owner");
    expect(wrapper.text()).toContain("管理员登录");
    expect(wrapper.text()).toContain("保存恢复码");
    expect(wrapper.text()).toContain(
      "abcde-fghij-klmno-pqrst-uvwxy-zabcd-efghi-jklmn",
    );

    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    await clickButton(wrapper, "复制恢复码");
    expect(writeText).toHaveBeenCalledWith(
      "abcde-fghij-klmno-pqrst-uvwxy-zabcd-efghi-jklmn",
    );
    expect(document.body.textContent).toContain("恢复码已复制。");

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
    await clickButton(wrapper, "服务");
    expect(wrapper.text()).toContain("共享 0 · 待审批 0 · 发现 关闭");
    await clickButton(wrapper, "客户端");

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
    await clickButton(wrapper, "客户端");

    await clickButton(wrapper, "扫描局域网");
    expect(wrapper.text()).toContain(
      "未发现服务器，请确认对方已启动服务且处于同一网段。",
    );

    await switchLanguage(wrapper, "en");
    expect(wrapper.text()).toContain(
      "No server found. Check that the other device is running PeerDav and on the same LAN.",
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
    await clickButton(wrapper, "客户端");

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
    expect(wrapper.text()).toContain("Docs");

    await clickButton(wrapper, "编辑");
    expect(wrapper.text()).toContain(
      "正在编辑“Docs”；名称和目录不可修改。",
    );

    const sharePane = wrapper.find("#pane-shares");
    const lockedTextInputs = sharePane
      .findAll('input[type="text"]')
      .filter((input) => input.element.disabled);
    expect(lockedTextInputs).toHaveLength(2);
    expect(lockedTextInputs[0].element.value).toBe("/srv/docs");
    expect(lockedTextInputs[1].element.value).toBe("Docs");

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

  it("places the directory first, derives its name, and defaults allow rules to the local subnet", async () => {
    const createShare = vi.fn(async (args) => args);
    const wrapper = await mountReady({
      local_networks: async () => ["10.0.5.0/24"],
      create_share: createShare,
    });
    await signIn(wrapper);
    await flushPromises();

    const form = wrapper.find("#pane-shares form");
    const inputs = form.findAll('input[type="text"]');
    expect(inputs[0].element.value).toBe("");
    expect(inputs[1].element.value).toBe("");

    await findButton(wrapper, "选择").trigger("click");
    await flushPromises();

    expect(inputs[0].element.value).toBe("/tmp/peerdav-keys");
    expect(inputs[1].element.value).toBe("peerdav-keys");

    await form.trigger("submit");
    await flushPromises();

    expect(createShare).toHaveBeenCalledWith(
      expect.objectContaining({
        name: "peerdav-keys",
        rootPath: "/tmp/peerdav-keys",
        rules: [
          {
            kind: "allow",
            matcher: { type: "network", value: "10.0.5.0/24" },
          },
        ],
      }),
    );

    wrapper.unmount();
  });
});

describe("home workspace", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("lists authorized servers and opens their file browser", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
    });

    expect(wrapper.text()).toContain("已连接的服务器");
    expect(wrapper.text()).toContain("Office");
    expect(wrapper.text()).toContain("http://192.168.1.10:4918");

    await wrapper.find("button.server-card").trigger("click");
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(true);
    expect(wrapper.text()).toContain("这个文件夹是空的。");

    wrapper.unmount();
  });

  it("unbinds an authorized server from the home list", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let savedPeers = [PEER];
    const forgetPeer = vi.fn(async () => {
      savedPeers = [];
    });
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => savedPeers,
      forget_peer: forgetPeer,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("Office");
    await clickButton(wrapper, "解绑");
    await flushPromises();

    expect(confirmAction).toHaveBeenCalledWith(
      "解绑服务器“Office”？服务器端授权将被取消，本机保存的授权码也会删除。",
      "解绑",
    );
    expect(forgetPeer).toHaveBeenCalledWith({ peerId: "peer-1" });
    expect(wrapper.text()).not.toContain("Office");

    wrapper.unmount();
  });

  it("does not unbind an authorized server when confirmation is cancelled", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const forgetPeer = vi.fn();
    confirmAction.mockResolvedValueOnce(false);
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
      forget_peer: forgetPeer,
    });
    await flushPromises();

    await clickButton(wrapper, "解绑");
    await flushPromises();

    expect(confirmAction).toHaveBeenCalled();
    expect(forgetPeer).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("Office");

    wrapper.unmount();
  });

  it("removes a saved server when its authorization is revoked", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let savedPeers = [PEER];
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => savedPeers,
    });
    await flushPromises();
    expect(wrapper.text()).toContain("Office");

    savedPeers = [];
    const handleRevoked = eventHandlers.get("app://peer-revoked");
    expect(typeof handleRevoked).toBe("function");
    handleRevoked({
      payload: {
        id: "peer-1",
        label: "Office",
        url: PEER.url,
        reason: "revoked",
      },
    });
    await flushPromises();

    expect(wrapper.text()).not.toContain("Office");
    expect(document.body.textContent).toContain("授权已取消");

    wrapper.unmount();
  });

  it("requests access and automatically adds the server after approval", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let savedPeers = [];
    const requestAccess = vi.fn(async () => ({
      requested: true,
      requestId: "request-1",
    }));
    const waitForAccess = vi.fn(async () => PEER);
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      scan_lan: async () => [DISCOVERED],
      list_peers: async () => savedPeers,
      request_peer_access: requestAccess,
      wait_for_peer_access: waitForAccess,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("局域网中的服务器");
    expect(wrapper.text()).toContain("Lab");
    expect(wrapper.text()).toContain("2 个共享");

    savedPeers = [PEER];
    await clickButton(wrapper, "申请访问");
    await flushPromises();

    expect(requestAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
    });
    expect(waitForAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
      requestId: "request-1",
      label: "Lab",
    });
    expect(wrapper.find(".peer-browser").exists()).toBe(false);

    wrapper.unmount();
  });

  it("shows the waiting state until the administrator approves access", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let savedPeers = [];
    let finishWaiting;
    const waitForAccess = vi.fn(
      () =>
        new Promise((resolve) => {
          finishWaiting = resolve;
        }),
    );
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      scan_lan: async () => [DISCOVERED],
      list_peers: async () => savedPeers,
      wait_for_peer_access: waitForAccess,
    });
    await flushPromises();

    await clickButton(wrapper, "申请访问");
    await flushPromises();

    expect(waitForAccess).toHaveBeenCalled();
    expect(wrapper.text()).toContain("已发送申请，等待对方管理员批准…");

    savedPeers = [PEER];
    finishWaiting(PEER);
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(false);

    wrapper.unmount();
  });

  it("cancels a pending access request from the discovered server row", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let rejectWaiting;
    const waitForAccess = vi.fn(
      () =>
        new Promise((_, reject) => {
          rejectWaiting = reject;
        }),
    );
    const cancelAccess = vi.fn(async () => {
      rejectWaiting({
        code: "PEER_ACCESS_CANCELED",
        message: "已取消访问申请 / The access request was canceled.",
      });
    });
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      scan_lan: async () => [DISCOVERED],
      list_peers: async () => [],
      wait_for_peer_access: waitForAccess,
      cancel_peer_access: cancelAccess,
    });
    await flushPromises();

    await clickButton(wrapper, "申请访问");
    await flushPromises();
    expect(wrapper.text()).toContain("已发送申请，等待对方管理员批准…");

    await clickButton(wrapper, "取消申请");
    await flushPromises();

    expect(cancelAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
      requestId: "request-1",
    });
    expect(wrapper.text()).toContain("已取消访问申请。");

    wrapper.unmount();
  });
});

describe("settings information architecture", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("opens Shares first and orders the tabs by everyday priority", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);

    const labels = wrapper
      .findAll(".settings-tab")
      .map((tab) => tab.text());
    expect(labels).toEqual(["共享", "客户端", "服务", "账户"]);
    expect(wrapper.find("#pane-shares").exists()).toBe(true);
    expect(wrapper.find("#pane-clients").exists()).toBe(false);

    wrapper.unmount();
  });

  it("shows approval as a message without a persistent code panel", async () => {
    let pending = [
      {
        id: "request-1",
        shareId: "share-1",
        shareName: "Docs",
        peerIp: "10.1.1.10",
        requestedPath: "/",
        attempts: 1,
      },
    ];
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_pending_requests: async () => pending,
      approve_pending_request: async () => {
        pending = [];
        return { authorizationCode: "code", autoDelivery: true };
      },
    });
    await flushPromises();
    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    await clickButton(wrapper, "客户端");

    expect(wrapper.text()).toContain("其他设备访问此设备");
    expect(wrapper.text()).toContain("此设备访问其他设备");
    expect(wrapper.text()).toContain("10.1.1.10");

    await clickButton(wrapper, "批准");
    await flushPromises();

    expect(wrapper.find(".code-box").exists()).toBe(false);
    expect(document.body.textContent).toContain(
      "已批准，客户端正在自动完成绑定。",
    );

    wrapper.unmount();
  });
});

describe("Finder-style peer browser", () => {
  const SHARE = {
    id: "share-1",
    name: "Docs",
    permissions: { canRead: true, canWrite: true, canDelete: false },
  };
  const LISTING = {
    shareId: "share-1",
    path: "/",
    entries: [
      {
        path: "/sub",
        name: "sub",
        isDir: true,
        size: 0,
        modified: 1700000000,
        contentType: null,
      },
      {
        path: "/notes.txt",
        name: "notes.txt",
        isDir: false,
        size: 11,
        modified: 1700000000,
        contentType: "text/plain",
      },
    ],
    truncated: false,
  };

  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  async function openBrowser(overrides = {}) {
    const downloadTo = vi.fn(async (args) => ({
      transferred: true,
      remotePath: args.remotePath,
      localPath: args.localPath,
      bytes: 11,
      skippedReason: null,
    }));
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
      list_peer_shares: async () => [SHARE],
      list_peer_dir: async () => LISTING,
      preview_peer_file: async () => ({
        fileName: "notes.txt",
        mime: "text/plain",
        encoding: "utf8",
        data: "hello peer\n",
        size: 11,
        truncated: false,
      }),
      download_peer_file_to: downloadTo,
      collect_upload_paths: async () => [
        {
          localPath: "/tmp/peerdav-keys",
          relativePath: "peerdav-keys",
          size: 5,
        },
      ],
      open_local_file: async () => {},
      reveal_local_file: async () => {},
      ...overrides,
    });
    await flushPromises();
    await wrapper.find("button.server-card").trigger("click");
    await flushPromises();
    return { wrapper, downloadTo };
  }

  it("switches between icons, list, columns and gallery views", async () => {
    const { wrapper } = await openBrowser();
    expect(wrapper.find(".icon-grid").exists()).toBe(true);
    expect(wrapper.text()).toContain("notes.txt");

    await wrapper.find('button[aria-label="列表"]').trigger("click");
    await flushPromises();
    expect(wrapper.find(".file-table").exists()).toBe(true);
    expect(wrapper.text()).toContain("修改时间");

    await wrapper.find('button[aria-label="分栏"]').trigger("click");
    await flushPromises();
    expect(wrapper.find(".column-view").exists()).toBe(true);

    await wrapper.find('button[aria-label="画廊"]').trigger("click");
    await flushPromises();
    expect(wrapper.find(".gallery-view").exists()).toBe(true);
    const thumbs = wrapper.findAll(".gallery-thumb");
    expect(thumbs).toHaveLength(2);
    await thumbs[1].trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("hello peer");

    wrapper.unmount();
  });

  it("shows download progress and the saved-file dialog", async () => {
    let finishDownload;
    const downloadTo = vi.fn(
      () =>
        new Promise((resolve) => {
          finishDownload = resolve;
        }),
    );
    const { wrapper } = await openBrowser({
      download_peer_file_to: downloadTo,
    });

    await wrapper.findAll(".file-tile")[1].trigger("click");
    await flushPromises();
    await clickButton(wrapper, "下载");
    await flushPromises();

    const transferId = downloadTo.mock.calls[0][0].transferId;
    const onProgress = eventHandlers.get("app://transfer-progress");
    expect(typeof onProgress).toBe("function");
    onProgress({
      payload: {
        transferId,
        direction: "download",
        fileName: "notes.txt",
        transferred: 5,
        total: 10,
      },
    });
    await flushPromises();

    expect(wrapper.text()).toContain("正在下载");
    expect(wrapper.text()).toContain("5 B");
    expect(wrapper.text()).toContain("10 B");

    finishDownload({
      transferred: true,
      remotePath: "/notes.txt",
      localPath: "/tmp/peerdav-save.txt",
      bytes: 10,
      skippedReason: null,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("下载完成");
    expect(wrapper.text()).toContain("打开所在目录");
    expect(wrapper.text()).toContain("打开文件");

    wrapper.unmount();
  });

  it("downloads to the native save path and asks before overwriting an upload", async () => {
    let uploadCalls = 0;
    const uploadPeerFile = vi.fn(async (args) => {
      uploadCalls += 1;
      if (uploadCalls === 1) {
        throw {
          code: "TRANSFER_CONFLICT",
          message: "目标已存在 / Destination already exists.",
        };
      }
      return {
        transferred: true,
        remotePath: `${args.remoteDir}/peerdav-keys`,
        localPath: null,
        bytes: 5,
        skippedReason: null,
      };
    });
    const { wrapper, downloadTo } = await openBrowser({
      upload_peer_file: uploadPeerFile,
    });

    await wrapper.findAll(".file-tile")[1].trigger("click");
    await flushPromises();
    await clickButton(wrapper, "下载");
    await flushPromises();

    expect(downloadTo).toHaveBeenCalledWith(
      expect.objectContaining({
        peerId: "peer-1",
        shareId: "share-1",
        remotePath: "/notes.txt",
        localPath: "/tmp/peerdav-save.txt",
        transferId: expect.any(String),
      }),
    );
    expect(wrapper.text()).not.toContain("本地下载目录");
    expect(wrapper.text()).toContain("下载完成");
    expect(wrapper.text()).toContain("文件已保存到 /tmp/peerdav-save.txt");

    await wrapper
      .find('button[aria-label="上传文件"]')
      .trigger("click");
    await flushPromises();

    expect(uploadPeerFile).toHaveBeenCalledTimes(2);
    expect(uploadPeerFile.mock.calls[0][0].conflict).toBe("fail");
    expect(uploadPeerFile.mock.calls[1][0].conflict).toBe("overwrite");
    expect(wrapper.text()).toContain("上传队列");
    expect(wrapper.text()).toContain("已完成");
    expect(confirmAction).toHaveBeenCalledWith(
      "服务器上已经存在“peerdav-keys”。要覆盖它吗？",
      "上传",
      { confirmText: "覆盖", cancelText: "跳过" },
    );

    wrapper.unmount();
  });

  it("keeps the browser open when a peer lookup fails transiently", async () => {
    let savedPeers = [PEER];
    const { wrapper } = await openBrowser({
      list_peers: async () => savedPeers,
      preview_peer_file: async () => {
        throw {
          code: "PEER_NOT_FOUND",
          message: "未找到该对端 / Peer not found.",
        };
      },
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await flushPromises();

    savedPeers = [];
    await clickButton(wrapper, "预览");
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(true);
    expect(wrapper.find(".icon-grid").exists()).toBe(true);

    wrapper.unmount();
  });

  it("preserves folder hierarchy when uploading a directory", async () => {
    const collectUploadPaths = vi.fn(async () => [
      {
        localPath: "/tmp/docs/a.txt",
        relativePath: "docs/a.txt",
        size: 3,
      },
      {
        localPath: "/tmp/docs/sub/b.txt",
        relativePath: "docs/sub/b.txt",
        size: 4,
      },
    ]);
    const uploadPeerFile = vi.fn(async (args) => ({
      transferred: true,
      remotePath: `${args.remoteDir}/file`,
      localPath: null,
      bytes: 3,
      skippedReason: null,
    }));
    const { wrapper } = await openBrowser({
      collect_upload_paths: collectUploadPaths,
      upload_peer_file: uploadPeerFile,
    });

    await wrapper
      .find('button[aria-label="上传文件夹"]')
      .trigger("click");
    await flushPromises();

    expect(collectUploadPaths).toHaveBeenCalledWith({
      paths: ["/tmp/peerdav-keys"],
    });
    expect(uploadPeerFile).toHaveBeenCalledTimes(2);
    expect(uploadPeerFile.mock.calls[0][0].remoteDir).toBe("/docs");
    expect(uploadPeerFile.mock.calls[1][0].remoteDir).toBe("/docs/sub");
    expect(wrapper.text()).toContain("docs/a.txt");
    expect(wrapper.text()).toContain("docs/sub/b.txt");

    wrapper.unmount();
  });
});

describe("titlebar appearance controls", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    initTheme();
    initAccent();
    invoke.mockReset();
  });

  it("renders localized labels in the titlebar menus", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    await wrapper
      .find('[data-menu="language"] .titlebar-menu__trigger')
      .trigger("click");
    await flushPromises();

    const labels = wrapper
      .findAll('[data-menu="language"] .titlebar-menu__item')
      .map((item) => item.text());
    expect(labels).toEqual(["简体中文", "English"]);

    wrapper.unmount();
  });

  it("switches the theme through the titlebar menu and persists it", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    await wrapper
      .find('[data-menu="theme"] .titlebar-menu__trigger')
      .trigger("click");
    await flushPromises();
    await wrapper
      .find('[data-menu="theme"] .titlebar-menu__item[data-value="dark"]')
      .trigger("click");
    await flushPromises();

    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(true);
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");

    wrapper.unmount();
  });

  it("switches the accent through the titlebar menu and persists it", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    await wrapper
      .find('[data-menu="accent"] .titlebar-menu__trigger')
      .trigger("click");
    await flushPromises();
    await wrapper
      .find('[data-menu="accent"] .titlebar-menu__item[data-value="purple"]')
      .trigger("click");
    await flushPromises();

    expect(document.documentElement.dataset.accent).toBe("purple");
    expect(localStorage.getItem(ACCENT_STORAGE_KEY)).toBe("purple");

    wrapper.unmount();
  });
});

describe("startup launch", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("reads and enables the OS autostart setting", async () => {
    const { enable, isEnabled } = await import(
      "@tauri-apps/plugin-autostart"
    );
    isEnabled.mockClear();
    enable.mockClear();

    const wrapper = await mountReady();
    await signIn(wrapper);
    await clickButton(wrapper, "服务");

    expect(isEnabled).toHaveBeenCalled();

    await wrapper.find(".settings-toggle .el-switch").trigger("click");
    await flushPromises();

    expect(enable).toHaveBeenCalledTimes(1);

    wrapper.unmount();
  });

  it("resumes the enabled server while the vault is locked", async () => {
    const wrapper = await mountReady({
      vault_status: async () => ({ initialized: true, unlocked: false }),
    });

    expect(invoke.mock.calls.map(([command]) => command)).toContain(
      "resume_server",
    );
    expect(wrapper.text()).toContain("加载保险箱密钥");
    expect(wrapper.text()).toContain("生成密钥");
    expect(wrapper.text()).not.toContain("重新加载密钥");

    wrapper.unmount();
  });

  it("offers key generation when the configured key file is invalid", async () => {
    const wrapper = await mountReady({
      vault_status: async () => ({
        initialized: true,
        unlocked: false,
        keyFile: "/tmp/empty.key",
        keyError: "invalid_key_file",
      }),
    });

    expect(wrapper.text()).toContain("生成密钥");
    expect(wrapper.text()).toContain("重新加载密钥");

    wrapper.unmount();
  });

  it("resets only after a recovery code and shows the next code", async () => {
    const reset = vi.fn(async () => ({
      adminUsername: "recovered",
      vaultStatus: { initialized: true, unlocked: true },
      recoveryCode: "new-recovery-code",
      archivedDatabase: "/tmp/peerdav.reset-1.pd",
    }));
    const wrapper = await mountReady({
      vault_status: async () => ({
        initialized: true,
        unlocked: false,
        keyFile: "/tmp/old.key",
        keyError: "key_file_missing",
      }),
      reset_vault: reset,
    });

    await clickButton(wrapper, "密钥丢失？重置保险箱");
    const form = wrapper.find("form.inset-form");
    const inputs = form.findAll("input");
    await inputs[0].setValue("old-recovery-code");
    await findButton(wrapper, "选择目录").trigger("click");
    await flushPromises();
    await inputs[2].setValue("recovered");
    await inputs[3].setValue("new admin password 2026");
    await inputs[4].setValue("new admin password 2026");
    await inputs[5].setValue("RESET");
    await form.trigger("submit");
    await flushPromises();

    expect(reset).toHaveBeenCalledWith({
      recoveryCode: "old-recovery-code",
      keyDirectory: "/tmp/peerdav-keys",
      adminUsername: "recovered",
      adminPassword: "new admin password 2026",
    });
    expect(wrapper.text()).toContain("管理员登录");
    expect(wrapper.text()).toContain("new-recovery-code");

    wrapper.unmount();
  });
});

describe("admin session restore", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    initTheme();
    initAccent();
    invoke.mockReset();
  });

  it("restores the signed-in admin after a reload", async () => {
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
    });

    expect(wrapper.text()).not.toContain("管理员登录");
    expect(wrapper.text()).toContain("首页");
    expect(wrapper.text()).toContain(
      "浏览已连接的服务器，或在局域网中申请新的访问。",
    );

    wrapper.unmount();
  });

  it("starts on Home for a returning admin after the first login", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");

    const wrapper = await mountReady();
    await signIn(wrapper);

    expect(wrapper.text()).toContain("首页");
    expect(wrapper.text()).not.toContain("当前管理员");
    expect(document.body.textContent).toContain("管理员登录成功。");

    wrapper.unmount();
  });

  it("shows Settings on the first login and remembers it", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);

    expect(wrapper.text()).toContain("当前管理员");
    expect(localStorage.getItem(SETTINGS_SEEN_STORAGE_KEY)).toBe("1");

    wrapper.unmount();
  });

  it("resumes the persisted server when the vault is already unlocked", async () => {
    const wrapper = await mountReady();

    expect(invoke.mock.calls.map(([command]) => command)).toContain(
      "resume_server",
    );

    wrapper.unmount();
  });

  it("stays on the sign-in screen when no admin session exists", async () => {
    const wrapper = await mountReady();

    expect(wrapper.text()).toContain("管理员登录");

    wrapper.unmount();
  });
});