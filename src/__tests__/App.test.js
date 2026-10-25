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
    local_networks: async () => ["192.168.1.0/24"],
    request_peer_access: async () => ({ requested: true }),
    connect_peer: async () => PEER,
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

    expect(wrapper.text()).toContain("已授权的服务器");
    expect(wrapper.text()).toContain("Office");
    expect(wrapper.text()).toContain("http://192.168.1.10:4918");

    await wrapper.find("button.server-card").trigger("click");
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(true);
    expect(wrapper.text()).toContain("目录为空。");

    wrapper.unmount();
  });

  it("shows discovered servers when none are authorized and requests access on add", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const requestAccess = vi.fn(async () => ({ requested: true }));
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      scan_lan: async () => [DISCOVERED],
      request_peer_access: requestAccess,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("发现的服务器");
    expect(wrapper.text()).toContain("Lab");
    expect(wrapper.text()).toContain("2 个共享");

    await clickButton(wrapper, "添加");
    await flushPromises();
    expect(requestAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
    });
    expect(wrapper.text()).toContain(
      "已发送访问请求，请在服务端批准后输入授权码。",
    );

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
    expect(wrapper.text()).toContain("已授权的服务器与文件访问入口");

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