import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { windowMock, eventHandlers, resizeHandlers } = vi.hoisted(() => {
  const eventHandlers = new Map();
  const resizeHandlers = new Set();
  const windowMock = {
    close: vi.fn(),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async (handler) => {
      resizeHandlers.add(handler);
      return () => resizeHandlers.delete(handler);
    }),
  };
  return { windowMock, eventHandlers, resizeHandlers };
});

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

afterEach(() => {
  window.history.replaceState(null, "", window.location.pathname);
  for (const message of document.querySelectorAll(".el-message")) {
    message.remove();
  }
  document.body.innerHTML = "";
});

function latestErrorMessage() {
  const messages = document.querySelectorAll(
    ".el-message--error .el-message__content",
  );
  return messages[messages.length - 1]?.textContent?.trim() ?? "";
}

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
  online: true,
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
    local_addresses: async () => ["192.168.1.23"],
    // 出站申请状态由后端保存；测试默认没有任何未完成的申请。
    access_requests: async () => [],
    inbound_connections: async () => [],
    outbound_connections: async () => [],
    device_fingerprint: async () => "a".repeat(64),
    request_peer_access: async ({ url, label }) => ({
      url,
      label: label ?? null,
      status: "waiting",
      message: null,
    }),
    wait_for_peer_access: async () => PEER,
    cancel_peer_access: async ({ url }) => ({
      url,
      label: null,
      status: "canceled",
      message: null,
    }),
    begin_peer_upload: async () => crypto.randomUUID(),
    cancel_peer_upload: async () => {},
    list_peer_shares: async () => [],
    admin_login: async () => ({
      username: "admin",
      mustChangePassword: false,
    }),
    admin_session: async () => null,
    list_shares: async () => [],
    list_pending_requests: async () => [],
    list_authorizations: async () => [],
    discovered_peers: async () => [],
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
  // Teleport targets are resolved through the document, so the app must be
  // attached while testing the titlebar file-action slot.
  const wrapper = mount(App, { attachTo: document.body });
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

    expect(latestErrorMessage()).toBe("请先选择密钥保存目录。");
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
    expect(latestErrorMessage()).toBe("两次输入的管理员密码不一致。");

    wrapper.unmount();
  });
});

describe("error rendering", () => {
  beforeEach(() => {
    localStorage.clear();
    initLocale();
    invoke.mockReset();
  });

  it("shows one localized half of a backend error as a temporary toast", async () => {
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

    expect(wrapper.find(".alert-error").exists()).toBe(false);
    expect(latestErrorMessage()).toBe("操作保险箱失败：保险箱已锁定，请先解锁");
    expect(document.querySelector(".el-message--error")?.style.top).toBe("54px");

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
    // 设置页的服务面板只留配置与动作，不再堆状态标签，也没有多余的刷新按钮。
    expect(wrapper.findAll("#pane-service .el-tag")).toHaveLength(0);
    expect(wrapper.text()).not.toContain("刷新列表");
    // 绑定地址改为三选一下拉，默认落在广域网（0.0.0.0），不再让用户手输地址。
    expect(wrapper.find("#pane-service .bind-select").exists()).toBe(true);
    expect(wrapper.find('#pane-service input[placeholder="0.0.0.0"]').exists()).toBe(
      false,
    );
    expect(wrapper.find("#pane-service").text()).toContain("广域网 0.0.0.0");
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

  it("shows the passive discovery empty state", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);
    await clickButton(wrapper, "客户端");

    // 空态使用与折叠面板同款外框，保证设计语言一致。
    const emptyBoxes = wrapper.findAll("#pane-clients .client-empty");
    expect(emptyBoxes).toHaveLength(2);
    expect(emptyBoxes[0].text()).toContain("暂无客户端");
    expect(emptyBoxes[1].text()).toContain("还没有可浏览的服务器");

    await switchLanguage(wrapper, "en");
    expect(wrapper.findAll("#pane-clients .client-empty")[1].text()).toContain(
      "No servers to browse yet",
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
    await signIn(wrapper, "admin-secret");

    expect(wrapper.text()).toContain("Docs");

    await wrapper.find('[aria-label="编辑"]').trigger("click");
    await flushPromises();
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
    await saveButton.trigger("click");
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

  it("lays each share row out as three columns with the arrow pinned right", async () => {
    const wrapper = await mountReady({ list_shares: async () => [SHARE] });
    await signIn(wrapper, "admin-secret");

    // Left = icon + name/address, middle = read/write/delete, right = actions.
    const columns = wrapper
      .find(".el-collapse-item__title")
      .findAll(":scope > *")
      .map((el) => el.classes().join(" "));
    expect(columns).toEqual([
      "share-item__main",
      "share-item__perms",
      "share-item__actions",
    ]);

    const headerChildren = wrapper
      .find(".el-collapse-item__header")
      .findAll(":scope > *")
      .map((el) => el.classes());
    expect(headerChildren.at(-1)).toContain("el-collapse-item__arrow");

    const [edit, remove] = wrapper
      .find(".share-item__actions")
      .findAll("button");
    for (const button of [edit, remove]) {
      expect(button.classes()).toEqual(
        expect.arrayContaining([
          "list-pill",
          "is-round",
          "el-button--small",
          "el-button--primary",
        ]),
      );
      expect(button.classes()).not.toContain("is-plain");
    }
    expect(edit.attributes("aria-label")).toBe("编辑");
    expect(remove.attributes("aria-label")).toBe("删除");

    // 权限列是自绘勾选框：授予=带勾的绿色框，未授予=灰色空框。
    // 用普通 span 而不是禁用的 el-checkbox，避免鼠标划过时出现 🚫 指针。
    const perms = wrapper.find(".share-item__perms").findAll(".perm-flag");
    expect(perms.map((flag) => flag.text())).toEqual(["读", "写", "删"]);
    expect(perms[0].classes()).toContain("is-granted");
    expect(perms[1].classes()).not.toContain("is-granted");
    expect(perms[0].attributes("role")).toBe("img");
    expect(perms[0].attributes("aria-label")).toBe("读：已授予");
    expect(perms[1].attributes("aria-label")).toBe("写：未授予");
    expect(
      wrapper.find(".share-item__perms").find(".el-checkbox").exists(),
    ).toBe(false);

    // 标题区的加号是面板级主操作：只有它保留强调色底，行内动作一律中性描边。
    const addButton = wrapper.find('[aria-label="创建共享"]');
    expect(addButton.classes()).toEqual(
      expect.arrayContaining([
        "list-pill",
        "pane-cta",
        "is-round",
        "el-button--primary",
      ]),
    );

    // The dialog form keeps the app's default button treatment: ordinary
    // rounded corners, not the list's pill size.
    await addButton.trigger("click");
    await flushPromises();
    const formButtons = [
      ...wrapper.findAll("#pane-shares form button"),
      ...wrapper.findAll("#pane-shares .el-dialog__footer button"),
    ];
    expect(formButtons).toHaveLength(3);
    for (const button of formButtons) {
      expect(button.classes()).not.toContain("is-round");
      expect(button.classes()).not.toContain("el-button--small");
    }

    wrapper.unmount();
  });

  it("keeps the share dialog open on overlay clicks and Escape", async () => {
    const wrapper = await mountReady({ list_shares: async () => [SHARE] });
    await signIn(wrapper, "admin-secret");

    // Element Plus hides the overlay with v-show instead of unmounting it.
    const overlay = () => wrapper.find("#pane-shares .el-overlay");
    const isClosed = () =>
      /display:\s*none/.test(overlay().attributes("style") ?? "");
    const dialogTitle = () =>
      wrapper.find("#pane-shares .el-dialog__title").text();
    // The leave transition resolves on the next frames, which jsdom only
    // advances when rAF is pumped manually.
    const settle = async () => {
      await flushPromises();
      await new Promise((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(resolve));
      });
      await flushPromises();
    };

    await wrapper.find('[aria-label="创建共享"]').trigger("click");
    await settle();

    expect(dialogTitle()).toBe("新建共享");
    expect(isClosed()).toBe(false);

    // align-center drops Element Plus's default `margin-top: 15vh`, which
    // pushed the form past the bottom edge of the 800x600 default window.
    const dialogEl = wrapper.find("#pane-shares .el-dialog");
    expect(dialogEl.classes()).toContain("is-align-center");
    expect(dialogEl.attributes("style") ?? "").not.toContain("15vh");

    // Neither the mask nor the keyboard shortcut may dismiss the form.
    await overlay().trigger("click");
    await wrapper
      .find("#pane-shares .el-dialog")
      .trigger("keydown.esc", { key: "Escape" });
    await settle();
    expect(isClosed()).toBe(false);

    // The footer cancel is the explicit close path.
    await findButton(wrapper, "取消").trigger("click");
    await settle();
    expect(isClosed()).toBe(true);

    // Editing reuses the same dialog with the edit title.
    await wrapper.find('[aria-label="编辑"]').trigger("click");
    await settle();
    expect(dialogTitle()).toBe("编辑共享");

    wrapper.unmount();
  });

  it("opens the create form from the header and derives the name from the directory", async () => {
    const createShare = vi.fn(async (args) => args);
    const wrapper = await mountReady({
      local_networks: async () => ["10.0.5.0/24"],
      create_share: createShare,
    });
    await signIn(wrapper);
    await flushPromises();

    // The create form stays hidden until the header add button is used.
    expect(wrapper.find("#pane-shares form").exists()).toBe(false);
    await wrapper.find('[aria-label="创建共享"]').trigger("click");
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

    // Detected networks are offered as choices but never preselected, so a new
    // share starts with no access rules until the user picks them.
    expect(createShare).toHaveBeenCalledWith(
      expect.objectContaining({
        name: "peerdav-keys",
        rootPath: "/tmp/peerdav-keys",
        rules: [],
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

  it("treats the home page as a console for incoming access requests", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let pending = [
      {
        id: "request-1",
        shareId: "share-1",
        shareName: "Docs",
        peerIp: "10.1.1.10",
        machineId: "a1b2c3d4e5f60718293a4b5c6d7e8f90",
        requestedPath: "/",
        attempts: 1,
      },
    ];
    const wrapper = await mountReady({
      list_pending_requests: async () => pending,
      approve_pending_request: async () => {
        pending = [];
        return { authorizationCode: "code", autoDelivery: true };
      },
    });
    await signIn(wrapper);

    // 控台排在最前面：别人的申请先出现，地址和机器码都列出来。
    const consoleSection = wrapper.findAll(".home-section")[0];
    expect(consoleSection.text()).toContain("待审批的访问申请");
    expect(consoleSection.findAll(".server-row")).toHaveLength(1);
    expect(consoleSection.text()).toContain("10.1.1.10");
    expect(consoleSection.text()).toContain("机器码 a1b2c3d4…8f90");
    expect(consoleSection.text()).toContain("Docs");

    await clickButton(consoleSection, "批准");

    // 没有待审批申请时整块隐藏，不留空态占位。
    expect(wrapper.text()).not.toContain("待审批的访问申请");
    expect(wrapper.text()).not.toContain("10.1.1.10");

    wrapper.unmount();
  });

  it("falls back to TCP discovery when UDP broadcasts are not forwarded", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let scanned = false;
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      scan_lan_tcp: async () => {
        scanned = true;
        return [DISCOVERED];
      },
      discovered_peers: async () => (scanned ? [DISCOVERED] : []),
    });
    await flushPromises();

    await clickButton(wrapper, "TCP 扫描");
    await flushPromises();

    expect(scanned).toBe(true);
    expect(wrapper.text()).toContain("Lab");
    expect(wrapper.text()).toContain("http://192.168.1.20:4918");

    wrapper.unmount();
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

    expect(wrapper.text()).toContain("已授权服务器");
    expect(wrapper.text()).toContain("Office");
    expect(wrapper.text()).toContain("http://192.168.1.10:4918");

    await wrapper.find("button.server-row__main").trigger("click");
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(true);
    expect(wrapper.text()).toContain("这个文件夹是空的。");

    wrapper.unmount();
  });

  it("reads each connected server's live share count for the home cards", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
      list_peer_shares: async () => [{ id: "a" }, { id: "b" }, { id: "c" }],
    });
    await flushPromises();

    expect(
      wrapper.find(".server-row .el-tag.row-tag").text(),
    ).toBe("共享 3 目录");

    wrapper.unmount();
  });

  it("disables an offline peer and removes it from the rail", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const listShares = vi.fn(async () => []);
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [{ ...PEER, online: false }],
      list_peer_shares: listShares,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("离线");
    // 离线的服务器读不到目录数，不再渲染「—」占位标签，只留失败状态标签。
    const offlineTags = wrapper.findAll(".server-row .el-tag");
    expect(offlineTags).toHaveLength(1);
    expect(offlineTags[0].text()).toBe("离线");
    expect(offlineTags[0].classes()).toContain("el-tag--danger");
    expect(wrapper.find("button.server-row__main").attributes("disabled")).toBeDefined();
    expect(wrapper.findAll(".rail-avatar")).toHaveLength(0);
    expect(listShares).not.toHaveBeenCalled();

    await wrapper.find("button.server-row__main").trigger("click");
    await flushPromises();
    expect(wrapper.find(".peer-browser").exists()).toBe(false);

    wrapper.unmount();
  });

  it("updates the rail and row when a peer status changes", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
    });
    await flushPromises();
    expect(wrapper.findAll(".rail-avatar")).toHaveLength(1);
    expect(wrapper.find("button.server-row__main").attributes("disabled")).toBeUndefined();

    const publish = eventHandlers.get("app://peer-status");
    expect(typeof publish).toBe("function");

    publish({ payload: { id: PEER.id, online: false } });
    await flushPromises();
    expect(wrapper.findAll(".rail-avatar")).toHaveLength(0);
    expect(wrapper.find("button.server-row__main").attributes("disabled")).toBeDefined();
    expect(wrapper.text()).toContain("离线");

    publish({ payload: { id: PEER.id, online: true } });
    await flushPromises();
    expect(wrapper.findAll(".rail-avatar")).toHaveLength(1);
    expect(wrapper.find("button.server-row__main").attributes("disabled")).toBeUndefined();

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
    let entries = [];
    const requestAccess = vi.fn(async ({ url, label }) => {
      const entry = { url, label: label ?? null, status: "waiting", message: null };
      entries = [entry];
      return { ...entry };
    });
    const waitForAccess = vi.fn(async () => {
      // 后端连接成功后会清掉这条申请，并保存对端。
      entries = [];
      savedPeers = [PEER];
      return PEER;
    });
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      access_requests: async () => entries,
      discovered_peers: async () => [DISCOVERED],
      list_peers: async () => savedPeers,
      request_peer_access: requestAccess,
      wait_for_peer_access: waitForAccess,
    });
    await flushPromises();

    expect(wrapper.text()).toContain("局域网中的服务器");
    expect(wrapper.text()).toContain("Lab");
    expect(
      wrapper.find(".server-row .el-tag.row-tag").text(),
    ).toBe("共享 2 目录");

    await clickButton(wrapper, "申请访问");
    await flushPromises();

    expect(requestAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
      label: "Lab",
    });
    expect(waitForAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
      label: "Lab",
    });
    expect(wrapper.find(".peer-browser").exists()).toBe(false);

    wrapper.unmount();
  });

  it("shows the waiting state until the administrator approves access", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let savedPeers = [];
    let entries = [];
    let finishWaiting;
    const requestAccess = vi.fn(async ({ url, label }) => {
      const entry = { url, label: label ?? null, status: "waiting", message: null };
      entries = [entry];
      return { ...entry };
    });
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
      access_requests: async () => entries,
      discovered_peers: async () => [DISCOVERED],
      list_peers: async () => savedPeers,
      request_peer_access: requestAccess,
      wait_for_peer_access: waitForAccess,
    });
    await flushPromises();

    await clickButton(wrapper, "申请访问");
    await flushPromises();

    expect(waitForAccess).toHaveBeenCalled();
    // 申请状态来自后端快照，重挂载后仍然保持「等待批准」。
    expect(wrapper.text()).toContain("已发送申请，等待对方管理员批准…");
    expect(wrapper.text()).toContain("取消申请");

    savedPeers = [PEER];
    finishWaiting(PEER);
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(false);

    wrapper.unmount();
  });

  it("reports sending only until the peer accepts the access request", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let entries = [];
    let resolveRequest;
    const requestAccess = vi.fn(
      ({ url, label }) =>
        new Promise((resolve) => {
          resolveRequest = () => {
            const entry = {
              url,
              label: label ?? null,
              status: "waiting",
              message: null,
            };
            entries = [entry];
            resolve(entry);
          };
        }),
    );
    const waitForAccess = vi.fn(() => new Promise(() => {}));
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      access_requests: async () => entries,
      discovered_peers: async () => [],
      list_peers: async () => [],
      request_peer_access: requestAccess,
      wait_for_peer_access: waitForAccess,
    });
    await flushPromises();
    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    await clickButton(wrapper, "客户端");

    await clickButton(wrapper, "手动输入");
    await wrapper
      .find(".manual-connect input")
      .setValue("http://192.168.1.99:4918");
    await wrapper.find(".manual-connect").trigger("submit");
    await flushPromises();

    // 请求还在飞：此时只能说「正在发送」，不能声称对方已经登记。
    expect(requestAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.99:4918",
      label: null,
    });
    expect(wrapper.find(".request-progress__notice").text()).toContain(
      "正在发送访问请求…",
    );
    expect(waitForAccess).not.toHaveBeenCalled();

    resolveRequest();
    await flushPromises();

    expect(waitForAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.99:4918",
      label: null,
    });
    expect(wrapper.find(".request-progress").text()).toContain(
      "已发送访问申请",
    );

    wrapper.unmount();
  });

  it("cancels a pending access request from the discovered server row", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let entries = [];
    let rejectWaiting;
    const requestAccess = vi.fn(async ({ url, label }) => {
      const entry = { url, label: label ?? null, status: "waiting", message: null };
      entries = [entry];
      return { ...entry };
    });
    const waitForAccess = vi.fn(
      () =>
        new Promise((_, reject) => {
          rejectWaiting = reject;
        }),
    );
    const cancelAccess = vi.fn(async ({ url }) => {
      entries = entries.map((entry) =>
        entry.url === url ? { ...entry, status: "canceled" } : entry,
      );
      rejectWaiting({
        code: "PEER_ACCESS_CANCELED",
        message: "已取消访问申请 / The access request was canceled.",
      });
      return { ...entries[0] };
    });
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      access_requests: async () => entries,
      discovered_peers: async () => [DISCOVERED],
      list_peers: async () => [],
      request_peer_access: requestAccess,
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
    });
    expect(wrapper.text()).toContain("已取消访问申请。");

    wrapper.unmount();
  });

  it("keeps a waiting access request across view switches and a reload", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    let entries = [
      {
        url: "http://192.168.1.20:4918",
        label: "Lab",
        status: "waiting",
        message: null,
      },
    ];
    const waitForAccess = vi.fn(() => new Promise(() => {}));
    const overrides = {
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      access_requests: async () => entries,
      discovered_peers: async () => [DISCOVERED],
      list_peers: async () => [],
      wait_for_peer_access: waitForAccess,
    };
    const wrapper = await mountReady(overrides);
    await flushPromises();

    // 后端已经记着这条申请：首屏就是「等待批准」，不是「可以申请」。
    expect(wrapper.text()).toContain("已发送申请，等待对方管理员批准…");
    expect(() => findButton(wrapper, "申请访问")).toThrow();

    // 切到设置页再回到首页：视图重建不影响状态。
    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    await wrapper.find(".rail-home").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("已发送申请，等待对方管理员批准…");
    wrapper.unmount();

    // 模拟 webview 重载：新的 App 实例从同一份后端快照恢复，并重新接上等待。
    waitForAccess.mockClear();
    const reloaded = await mountReady({ ...overrides, access_requests: async () => entries });
    await flushPromises();
    expect(reloaded.text()).toContain("已发送申请，等待对方管理员批准…");
    expect(waitForAccess).toHaveBeenCalledWith({
      url: "http://192.168.1.20:4918",
      label: "Lab",
    });
    reloaded.unmount();
  });

  it("re-enters settings with the cached lists instead of flashing a skeleton", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_shares: async () => [
        {
          id: "share-1",
          name: "Docs",
          rootPath: "/srv/docs",
          permissions: { canRead: true, canWrite: false, canDelete: false },
          rules: [],
        },
      ],
    });
    await flushPromises();

    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("Docs");

    await wrapper.find(".rail-home").trigger("click");
    await flushPromises();
    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();

    // 数据已经缓存在共享上下文里：重新进入时直接渲染旧列表，不再闪骨架屏。
    expect(wrapper.findAll(".list-skeleton")).toHaveLength(0);
    expect(wrapper.text()).toContain("Docs");

    wrapper.unmount();
  });

  it("adds and removes LAN servers from passive discovery events", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [],
    });
    await flushPromises();

    const publish = eventHandlers.get("app://discovered-peers");
    expect(typeof publish).toBe("function");

    publish({ payload: [DISCOVERED] });
    await flushPromises();
    expect(wrapper.text()).toContain("Lab");

    publish({ payload: [] });
    await flushPromises();
    expect(wrapper.text()).not.toContain("Lab");

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

    // 折叠头只保留标识与状态，请求路径等细节必须展开后才出现。
    const inboundHeader = wrapper.find(
      "#pane-clients .client-collapse .el-collapse-item__header",
    );
    expect(inboundHeader.text()).toContain("10.1.1.10");
    expect(inboundHeader.text()).toContain("待审批");
    expect(inboundHeader.text()).not.toContain("请求路径");

    await inboundHeader.trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("请求路径");

    await clickButton(wrapper, "审批");
    await flushPromises();

    expect(wrapper.find(".code-box").exists()).toBe(false);
    expect(document.body.textContent).toContain(
      "已审批，客户端正在自动完成绑定。",
    );

    wrapper.unmount();
  });

  it("holds list skeletons until the first data arrives", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const pending = { list_shares: [], list_peers: [], discovered_peers: [] };
    const defer = (key) => () =>
      new Promise((resolve) => {
        pending[key].push(resolve);
      });
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_shares: defer("list_shares"),
      list_peers: defer("list_peers"),
      discovered_peers: defer("discovered_peers"),
    });
    await flushPromises();

    // 首页两张列表：已授权服务器、局域网中的服务器。数据没到之前不能显示空态。
    const homeSkeletons = wrapper.findAll(".list-skeleton");
    expect(homeSkeletons).toHaveLength(2);
    expect(homeSkeletons[0].findAll(".list-skeleton__row")).toHaveLength(2);
    expect(homeSkeletons[1].findAll(".list-skeleton__row")).toHaveLength(3);
    expect(homeSkeletons[0].find(".el-skeleton__item").exists()).toBe(true);
    expect(homeSkeletons[0].attributes("role")).toBe("status");
    expect(wrapper.findAll(".server-list")).toHaveLength(0);

    // 设置页共享策略列表同样等待首屏数据。
    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    expect(wrapper.findAll(".list-skeleton")).toHaveLength(1);

    // 客户端两个折叠面板：入站列表等管理员数据，出站列表等发现快照与已保存对等端。
    await clickButton(wrapper, "客户端");
    expect(wrapper.findAll(".list-skeleton")).toHaveLength(2);

    for (const resolvers of Object.values(pending)) {
      for (const resolve of resolvers) resolve([]);
    }
    await flushPromises();

    expect(wrapper.findAll(".list-skeleton")).toHaveLength(0);
    expect(wrapper.findAll(".client-empty")).toHaveLength(2);

    await clickButton(wrapper, "共享");
    await flushPromises();
    expect(wrapper.findAll(".list-skeleton")).toHaveLength(0);
    expect(wrapper.text()).toContain("暂无共享目录。");

    wrapper.unmount();
  });

  it("renders list counts as heading badges and statuses as semantic tags", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
      list_pending_requests: async () => [
        {
          id: "request-1",
          shareId: "share-1",
          shareName: "Docs",
          peerIp: "10.1.1.10",
          requestedPath: "/",
          attempts: 1,
        },
      ],
      list_authorizations: async () => [
        { id: "auth-1", label: "10.1.1.11", createdAt: Date.now() },
      ],
    });
    await flushPromises();

    // 标题右侧是数量角标，不再是「共 N 台」的说明文字。
    expect(
      wrapper.findAll(".section-heading .count-badge").map((badge) => badge.text()),
    ).toEqual(["1"]);
    // 发现关闭属于中性未开启状态，用灰色而不是红色。
    const discoveryChip = wrapper.find(".section-heading .el-tag");
    expect(discoveryChip.text()).toBe("发现已关闭");
    expect(discoveryChip.classes()).toContain("el-tag--info");
    // 数量是中性事实，用 info 灰而不是品牌色。
    const countChip = wrapper.find(".server-row .el-tag");
    expect(countChip.text()).toBe("共享 0 目录");
    expect(countChip.classes()).toContain("el-tag--info");

    await wrapper.find('button[aria-label="打开设置"]').trigger("click");
    await flushPromises();
    await clickButton(wrapper, "客户端");

    expect(
      wrapper
        .findAll(".role-heading .count-badge")
        .map((badge) => badge.text()),
    ).toEqual(["2", "1"]);

    const chips = wrapper.findAll(".client-item__status .el-tag");
    expect(chips.map((chip) => chip.text())).toEqual([
      "待审批",
      "已审批",
      "已审批",
    ]);
    // 待审批要用户动手，用琥珀色提醒；已审批是成功态，用绿色。
    expect(chips[0].classes()).toContain("el-tag--warning");
    expect(chips[1].classes()).toContain("el-tag--success");
    expect(chips[2].classes()).toContain("el-tag--success");

    // 行内动作保留主类型按钮的语义，但配色降为中性描边（由 list-pill 覆盖），
    // 只有面板级主按钮用 pane-cta 拿回强调色底。
    const rowButtons = wrapper.findAll(".client-item__actions button");
    expect(rowButtons.length).toBeGreaterThan(0);
    for (const button of rowButtons) {
      expect(button.classes()).toEqual(
        expect.arrayContaining(["list-pill", "is-round", "el-button--primary"]),
      );
      expect(button.classes()).not.toContain("is-plain");
      expect(button.classes()).not.toContain("pane-cta");
    }

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
    await wrapper.find("button.server-row__main").trigger("click");
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

  it("restores ancestor columns and scrolls to the newest column", async () => {
    const subListing = {
      ...LISTING,
      path: "/sub",
      entries: [
        {
          ...LISTING.entries[1],
          path: "/sub/inner.txt",
          name: "inner.txt",
        },
      ],
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async ({ path }) => (path === "/sub" ? subListing : LISTING),
    });

    await wrapper.find('button[aria-label="列表"]').trigger("click");
    await flushPromises();
    await wrapper.findAll(".file-table tbody tr")[0].trigger("dblclick");
    await flushPromises();

    await wrapper.find('button[aria-label="分栏"]').trigger("click");
    await flushPromises();

    const columns = wrapper.findAll(".column");
    expect(columns).toHaveLength(2);
    expect(columns[0].text()).toContain("notes.txt");
    expect(columns[1].text()).toContain("inner.txt");

    const columnView = wrapper.find(".column-view").element;
    Object.defineProperty(columnView, "scrollWidth", {
      configurable: true,
      value: 720,
    });
    await columns[0].findAll(".column-row")[0].trigger("click");
    await flushPromises();

    expect(columnView.scrollLeft).toBe(720);

    wrapper.unmount();
  });

  it("renders audio and video previews with playable elements", async () => {
    const mediaEntries = [
      {
        ...LISTING.entries[1],
        path: "/song.mp3",
        name: "song.mp3",
        contentType: null,
      },
      {
        ...LISTING.entries[1],
        path: "/clip.mp4",
        name: "clip.mp4",
        contentType: null,
      },
    ];
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], ...mediaEntries],
      }),
      preview_peer_file: async ({ remotePath }) =>
        remotePath === "/song.mp3"
          ? {
              fileName: "song.mp3",
              mime: "audio/mpeg",
              encoding: "base64",
              data: "SUQz",
              size: 3,
            }
          : {
              fileName: "clip.mp4",
              mime: "video/mp4",
              encoding: "base64",
              data: "AAAA",
              size: 3,
            },
    });

    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");
    expect(wrapper.find(".preview-dialog audio").attributes("src")).toBe(
      "data:audio/mpeg;base64,SUQz",
    );

    await clickButton(wrapper, "关闭");
    await wrapper.findAll(".file-tile")[2].trigger("click");
    await clickButton(wrapper, "预览");
    expect(wrapper.find(".preview-dialog video").attributes("src")).toBe(
      "data:video/mp4;base64,AAAA",
    );

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

  it("opens text previews in a full-browser dialog", async () => {
    const { wrapper } = await openBrowser();
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    expect(wrapper.find(".preview-dialog").exists()).toBe(true);
    expect(wrapper.find(".preview-panel").exists()).toBe(false);
    expect(wrapper.find(".preview-text-full").text()).toContain("hello peer");

    wrapper.unmount();
  });

  it("renders Markdown previews instead of raw source", async () => {
    const markdownEntry = {
      ...LISTING.entries[1],
      path: "/README.md",
      name: "README.md",
      contentType: "text/plain",
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], markdownEntry],
      }),
      preview_peer_file: async () => ({
        fileName: "README.md",
        mime: "text/plain",
        encoding: "utf8",
        data: "# Hello\n\nSome **bold** text.",
        size: 28,
      }),
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    await vi.waitFor(() => {
      expect(wrapper.find(".markdown-body h1").exists()).toBe(true);
    });
    expect(wrapper.find(".markdown-body h1").text()).toBe("Hello");
    expect(wrapper.find(".markdown-body strong").text()).toBe("bold");
    expect(wrapper.find(".preview-text-full").exists()).toBe(false);

    wrapper.unmount();
  });

  it("renders CSV and TSV previews as data tables", async () => {
    const tableEntry = {
      ...LISTING.entries[1],
      path: "/people.tsv",
      name: "people.tsv",
      contentType: "text/tab-separated-values",
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], tableEntry],
      }),
      preview_peer_file: async () => ({
        fileName: "people.tsv",
        mime: "text/tab-separated-values",
        encoding: "utf8",
        data: "name\tage\nAda\t36\nLin\t28\n",
        size: 24,
      }),
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    const headers = wrapper
      .findAll(".preview-table thead th")
      .map((cell) => cell.text());
    expect(headers).toEqual(["#", "name", "age"]);
    expect(wrapper.find(".preview-table tbody").text()).toContain("Ada");
    expect(wrapper.find(".preview-table tbody").text()).toContain("36");

    wrapper.unmount();
  });

  it("highlights code previews", async () => {
    const codeEntry = {
      ...LISTING.entries[1],
      path: "/main.rs",
      name: "main.rs",
      contentType: "text/plain",
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], codeEntry],
      }),
      preview_peer_file: async () => ({
        fileName: "main.rs",
        mime: "text/plain",
        encoding: "utf8",
        data: "fn main() {}\n",
        size: 13,
      }),
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    await vi.waitFor(() => {
      expect(wrapper.find(".preview-code .hljs-keyword").exists()).toBe(true);
    });
    expect(wrapper.find(".preview-code .hljs-keyword").text()).toBe("fn");
    expect(
      wrapper.findAll(".preview-code-gutter span").map((line) => line.text()),
    ).toEqual(["1", "2"]);

    wrapper.unmount();
  });

  it("opens previews on double click in list and column views", async () => {
    const { wrapper } = await openBrowser();

    await wrapper.find('button[aria-label="列表"]').trigger("click");
    await flushPromises();
    const listRow = wrapper
      .findAll(".file-table tbody tr")
      .find((row) => row.text().includes("notes.txt"));
    await listRow.trigger("dblclick");
    await flushPromises();
    expect(wrapper.find(".preview-dialog").exists()).toBe(true);

    await clickButton(wrapper, "关闭");
    await wrapper.find('button[aria-label="分栏"]').trigger("click");
    await flushPromises();
    const columnRow = wrapper
      .findAll(".column-row")
      .find((row) => row.text().includes("notes.txt"));
    await columnRow.trigger("dblclick");
    await flushPromises();
    expect(wrapper.find(".preview-dialog").exists()).toBe(true);

    wrapper.unmount();
  });

  it("skips online previews above the size limit", async () => {
    const largeEntry = {
      ...LISTING.entries[1],
      path: "/large.txt",
      name: "large.txt",
      size: 10 * 1024 * 1024 + 1,
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], largeEntry],
      }),
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    expect(
      invoke.mock.calls.some(([command]) => command === "preview_peer_file"),
    ).toBe(false);
    expect(wrapper.find(".preview-unavailable").text()).toContain(
      "无法在线预览",
    );

    wrapper.unmount();
  });

  it("shows the line-limit reason returned by the backend", async () => {
    const { wrapper } = await openBrowser({
      preview_peer_file: async () => {
        throw {
          code: "PEER_PREVIEW_TOO_MANY_LINES",
          message: "文件超过在线预览行数限制 / The file exceeds the online preview line limit.",
          context: { lines: 5001, limit: 5000 },
        };
      },
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await clickButton(wrapper, "预览");

    expect(wrapper.find(".preview-unavailable").text()).toContain("5001");
    expect(wrapper.find(".preview-unavailable").text()).toContain("5000");

    wrapper.unmount();
  });

  it("hides preview and external edit actions for binary files", async () => {
    const binaryEntry = {
      ...LISTING.entries[1],
      path: "/archive.zip",
      name: "archive.zip",
      contentType: "application/zip",
    };
    const { wrapper } = await openBrowser({
      list_peer_dir: async () => ({
        ...LISTING,
        entries: [LISTING.entries[0], binaryEntry],
      }),
    });
    await wrapper.findAll(".file-tile")[1].trigger("click");
    await flushPromises();

    const labels = wrapper
      .findAll(".selection-actions button")
      .map((button) => button.text());
    expect(labels).toContain("下载");
    expect(labels).not.toContain("预览");
    expect(labels).not.toContain("编辑");

    wrapper.unmount();
  });

  it("deletes a selected folder only after confirmation", async () => {
    const remove = vi.fn(async () => {});
    const { wrapper } = await openBrowser({
      delete_peer_entry: remove,
      list_peer_shares: async () => [
        { ...SHARE, permissions: { ...SHARE.permissions, canDelete: true } },
      ],
    });
    await wrapper.findAll(".file-tile")[0].trigger("click");
    await flushPromises();

    await clickButton(wrapper, "删除");

    expect(confirmAction).toHaveBeenCalledWith(
      "删除文件夹“sub”及其中的所有内容？此操作无法撤销。",
      "删除",
    );
    expect(remove).toHaveBeenCalledWith({
      peerId: "peer-1",
      shareId: "share-1",
      remotePath: "/sub",
    });
    expect(document.body.textContent).toContain("已删除“sub”。");

    wrapper.unmount();
  });

  it("keeps the entry when deletion is canceled", async () => {
    const remove = vi.fn(async () => {});
    confirmAction.mockResolvedValueOnce(false);
    const { wrapper } = await openBrowser({
      delete_peer_entry: remove,
      list_peer_shares: async () => [
        { ...SHARE, permissions: { ...SHARE.permissions, canDelete: true } },
      ],
    });
    await wrapper.findAll(".file-tile")[0].trigger("click");
    await flushPromises();

    await clickButton(wrapper, "删除");

    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(remove).not.toHaveBeenCalled();

    wrapper.unmount();
  });

  it("hides deletion when the share does not allow it", async () => {
    const { wrapper } = await openBrowser({
      list_peer_shares: async () => [
        {
          ...SHARE,
          permissions: { ...SHARE.permissions, canDelete: false },
        },
      ],
    });
    await wrapper.findAll(".file-tile")[0].trigger("click");
    await flushPromises();

    expect(findButton(wrapper, "打开").exists()).toBe(true);
    expect(
      wrapper.findAll(".selection-actions button").some((button) => button.text() === "删除"),
    ).toBe(false);

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

  it("downloads a folder through the queue with aggregate progress", async () => {
    let finishFirst;
    const downloadTo = vi.fn((args) => {
      if (args.remotePath === "/sub/inner.txt") {
        return new Promise((resolve) => {
          finishFirst = resolve;
        });
      }
      return Promise.resolve({
        transferred: true,
        remotePath: args.remotePath,
        localPath: args.localPath,
        bytes: 3,
        skippedReason: null,
      });
    });
    const prepare = vi.fn(async () => ({
      localRoot: "/tmp/downloads/sub",
      directoryCount: 2,
      files: [
        {
          remotePath: "/sub/inner.txt",
          relativePath: "inner.txt",
          localPath: "/tmp/downloads/sub/inner.txt",
          size: 4,
        },
        {
          remotePath: "/sub/deep/a.txt",
          relativePath: "deep/a.txt",
          localPath: "/tmp/downloads/sub/deep/a.txt",
          size: 3,
        },
      ],
    }));
    const { wrapper } = await openBrowser({
      prepare_peer_directory_download: prepare,
      download_peer_file_to: downloadTo,
    });

    await wrapper.findAll(".file-tile")[0].trigger("click");
    await flushPromises();
    await clickButton(wrapper, "下载");
    await flushPromises();

    expect(prepare).toHaveBeenCalledWith({
      peerId: "peer-1",
      shareId: "share-1",
      remotePath: "/sub",
      localParent: "/tmp/peerdav-keys",
      conflict: "fail",
    });
    expect(wrapper.text()).toContain("下载队列");
    expect(wrapper.text()).toContain("/tmp/downloads/sub");
    expect(wrapper.text()).toContain("inner.txt");
    expect(downloadTo).toHaveBeenCalledTimes(1);

    const firstTransfer = downloadTo.mock.calls[0][0].transferId;
    const onProgress = eventHandlers.get("app://transfer-progress");
    onProgress({
      payload: {
        transferId: firstTransfer,
        direction: "download",
        fileName: "inner.txt",
        transferred: 2,
        total: 4,
      },
    });
    await flushPromises();
    expect(wrapper.text()).toContain("2 B / 7 B");

    finishFirst({
      transferred: true,
      remotePath: "/sub/inner.txt",
      localPath: "/tmp/downloads/sub/inner.txt",
      bytes: 4,
      skippedReason: null,
    });
    await vi.waitFor(() => {
      expect(downloadTo).toHaveBeenCalledTimes(2);
      expect(wrapper.text()).toContain("已处理 2/2 个文件");
    });
    expect(document.body.textContent).toContain("已下载 2 个文件。");

    wrapper.unmount();
  });

  it("asks before overwriting a local download folder", async () => {
    const plan = {
      localRoot: "/tmp/downloads/sub",
      directoryCount: 1,
      files: [
        {
          remotePath: "/sub/inner.txt",
          relativePath: "inner.txt",
          localPath: "/tmp/downloads/sub/inner.txt",
          size: 4,
        },
      ],
    };
    const prepare = vi
      .fn()
      .mockRejectedValueOnce({
        code: "TRANSFER_CONFLICT",
        message: "目标文件已存在",
        detail: null,
        context: null,
      })
      .mockResolvedValueOnce(plan);
    const { wrapper } = await openBrowser({
      prepare_peer_directory_download: prepare,
      download_peer_file_to: vi.fn(async (args) => ({
        transferred: true,
        remotePath: args.remotePath,
        localPath: args.localPath,
        bytes: 4,
        skippedReason: null,
      })),
    });

    await wrapper.findAll(".file-tile")[0].trigger("click");
    await flushPromises();
    await clickButton(wrapper, "下载");
    await flushPromises();

    expect(confirmAction).toHaveBeenCalledWith(
      expect.stringContaining("sub"),
      "下载",
      expect.objectContaining({ confirmText: "覆盖", cancelText: "取消" }),
    );
    expect(prepare).toHaveBeenLastCalledWith(
      expect.objectContaining({ conflict: "overwrite" }),
    );
    expect(wrapper.text()).toContain("已处理 1/1 个文件");

    wrapper.unmount();
  });

  it("shows per-file upload progress and speed and cancels the active file", async () => {
    let rejectUpload;
    const upload = vi.fn(() => new Promise((_, reject) => { rejectUpload = reject; }));
    const cancel = vi.fn(async () => {
      rejectUpload({ code: "TRANSFER_CANCELED", message: "已取消文件传输" });
    });
    const { wrapper } = await openBrowser({
      begin_peer_upload: async () => "upload-current",
      upload_peer_file: upload,
      cancel_peer_upload: cancel,
      collect_upload_paths: async () => [
        { localPath: "/tmp/large.bin", relativePath: "large.bin", size: 700 * 1024 * 1024 },
        { localPath: "/tmp/next.bin", relativePath: "next.bin", size: 1 },
      ],
    });
    await wrapper.find('button[aria-label="上传文件"]').trigger("click");
    await flushPromises();
    expect(wrapper.find(".upload-current").text()).toContain("正在校验文件");
    const progress = eventHandlers.get("app://transfer-progress");
    progress({ payload: { transferId: "unrelated", direction: "upload", phase: "uploading", total: 1, transferred: 1 } });
    progress({ payload: { transferId: "upload-current", direction: "upload", phase: "uploading",
      total: 700 * 1024 * 1024, transferred: 350 * 1024 * 1024, bytesPerSecond: 12 * 1024 * 1024 } });
    await flushPromises();
    expect(wrapper.find(".upload-current").text()).toContain("50%");
    expect(wrapper.find(".upload-current").text()).toContain("12.0 MB/s");
    expect(wrapper.find(".upload-current").text()).toContain("350.0 MB / 700.0 MB");
    await clickButton(wrapper, "取消上传");
    expect(cancel).toHaveBeenCalledWith({ transferId: "upload-current" });
    expect(upload).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).not.toContain("正在取消");
    expect(wrapper.findAll(".transfer-queue .el-tag").map((tag) => tag.text())).toEqual(["已取消", "已取消"]);
    expect(latestErrorMessage()).toBe("");
    progress({ payload: { transferId: "upload-current", direction: "upload", phase: "uploading", total: 1, transferred: 1 } });
    await flushPromises();
    expect(wrapper.find(".upload-current").exists()).toBe(false);
    wrapper.unmount();
  });

  it("keeps upload completion below 100 percent until the atomic save finishes", async () => {
    let finish;
    const cancel = vi.fn(async () => {});
    const { wrapper } = await openBrowser({
      begin_peer_upload: async () => "upload-save",
      upload_peer_file: () => new Promise((resolve) => { finish = resolve; }),
      cancel_peer_upload: cancel,
    });
    await wrapper.find('button[aria-label="上传文件"]').trigger("click");
    await flushPromises();
    eventHandlers.get("app://transfer-progress")({ payload: {
      transferId: "upload-save", direction: "upload", phase: "committing", transferred: 5, total: 5, bytesPerSecond: 0,
    } });
    await flushPromises();
    expect(wrapper.find(".upload-current").text()).toContain("正在合并并保存");
    expect(wrapper.text()).toContain("99%");
    await clickButton(wrapper, "取消上传");
    expect(cancel).toHaveBeenCalledWith({ transferId: "upload-save" });
    expect(wrapper.text()).toContain("正在取消");
    // If atomic replacement already won, the backend returns its true result.
    finish({ transferred: true, bytes: 5 });
    await flushPromises();
    expect(wrapper.text()).toContain("100%");
    expect(wrapper.text()).not.toContain("正在取消");
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

  it("shows the file actions in the titlebar only while the browser is open", async () => {
    const { wrapper } = await openBrowser();

    const slot = wrapper.find("#titlebar-file-actions");
    expect(slot.exists()).toBe(true);
    expect(slot.findAll(".view-switch")).toHaveLength(2);
    expect(slot.findAll(".view-switch__button")).toHaveLength(7);
    expect(slot.findAll(".titlebar-divider")).toHaveLength(1);
    expect(slot.find(".pathbar-actions").classes()).not.toContain(
      "titlebar-pill",
    );
    expect(wrapper.find(".titlebar-pill").exists()).toBe(false);
    expect(wrapper.find(".pathbar .pathbar-actions").exists()).toBe(false);
    // The appearance controls stay in the titlebar next to the file actions.
    expect(wrapper.find(".titlebar .titlebar-actions").exists()).toBe(true);
    expect(
      wrapper.find(".titlebar-slot--end > .titlebar-divider").exists(),
    ).toBe(true);

    await wrapper.find(".rail-home").trigger("click");
    await flushPromises();

    expect(wrapper.find(".peer-browser").exists()).toBe(false);
    expect(
      wrapper.find("#titlebar-file-actions").element.children,
    ).toHaveLength(0);

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

  it("uses a translation icon for the language menu", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    const trigger = wrapper.find(
      '[data-menu="language"] .titlebar-menu__trigger',
    );
    const icon = trigger.find("svg");
    expect(icon.exists()).toBe(true);
    expect(icon.find("path").classes()).toContain("titlebar-icon-fill");
    expect(trigger.text()).toBe("");

    wrapper.unmount();
  });

  it("keeps the titlebar free of a brand label and file actions outside the browser", async () => {
    const wrapper = await mountReady(VAULT_NOT_INITIALIZED);

    expect(wrapper.find(".titlebar-title").exists()).toBe(false);
    const appearance = wrapper.find(".titlebar .titlebar-actions");
    expect(appearance.exists()).toBe(true);
    expect(wrapper.find(".titlebar-pill").exists()).toBe(false);
    expect(
      wrapper.find("#titlebar-file-actions").element.children,
    ).toHaveLength(0);

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

  it("restores the service name and saves explicit TCP discovery networks", async () => {
    const saved = vi.fn(async () => null);
    const wrapper = await mountReady({
      service_settings: async () => ({ name: "开发主机", bindAddress: "0.0.0.0", port: 4918, discoveryPort: 4919,
        tcp: { enabled: true, networks: ["10.2.2.0/24"] } }),
      set_tcp_discovery: saved,
    });
    await signIn(wrapper);
    await clickButton(wrapper, "服务");
    expect(wrapper.find('input[placeholder="PeerDav"]').element.value).toBe("开发主机");
    expect(wrapper.find(".tcp-discovery-settings .el-switch").classes()).toContain("is-checked");
    await clickButton(wrapper, "保存探测设置");
    expect(saved).toHaveBeenCalled();
    const call = invoke.mock.calls.find(([command]) => command === "set_tcp_discovery");
    expect(call[1].settings).toEqual({ enabled: true, networks: ["10.2.2.0/24"] });
    wrapper.unmount();
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

    await wrapper.find(".settings-toggle:not(.tcp-discovery-settings) .el-switch").trigger("click");
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

  it("clips all dialog overlays below the draggable titlebar and follows window geometry", async () => {
    const source = readFileSync("src/App.vue", "utf8");
    expect(source).toContain("body:has(.window-frame) > .el-overlay");
    expect(source).toContain("top: calc(var(--titlebar-height) + var(--overlay-inset))");
    expect(source).toContain("border-radius: 0 0 var(--overlay-radius) var(--overlay-radius)");
    expect(source).toContain("clip-path: inset(0 round 0 0 var(--overlay-radius) var(--overlay-radius))");
    expect(source).toMatch(/\.el-overlay-dialog\s*\{\s*position: absolute;\s*inset: 0;/);
    const wrapper = await mountReady();
    expect(wrapper.find(".titlebar").attributes()).toHaveProperty("data-tauri-drag-region");
    expect(wrapper.find(".window-frame").attributes("style")).toContain("--window-overlay-inset: 1px");
    windowMock.isMaximized.mockResolvedValueOnce(true);
    for (const handler of resizeHandlers) handler();
    await flushPromises();
    expect(wrapper.find(".window-frame").attributes("style")).toContain("--window-overlay-radius: 0px");
    wrapper.unmount();
  });

  it("squares the window corners when the window is maximized", async () => {
    windowMock.isMaximized.mockResolvedValueOnce(true);
    const wrapper = await mountReady();
    expect(wrapper.find(".window-frame").classes()).toContain("is-maximized");
    wrapper.unmount();
  });

  it("restores the signed-in admin after a reload", async () => {
    const wrapper = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
    });

    expect(wrapper.text()).not.toContain("管理员登录");
    expect(wrapper.text()).toContain("局域网中的服务器");

    wrapper.unmount();
  });

  it("keeps the open server page after a webview reload", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");
    const overrides = {
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
      list_peers: async () => [PEER],
    };
    const wrapper = await mountReady(overrides);
    await wrapper.find("button.server-row__main").trigger("click");
    await flushPromises();

    expect(window.location.hash).toBe("#/peer/peer-1");
    expect(wrapper.find(".peer-browser").exists()).toBe(true);
    wrapper.unmount();

    const restored = await mountReady(overrides);
    expect(restored.find(".peer-browser").exists()).toBe(true);
    restored.unmount();
  });

  it("keeps the selected settings tab after a webview reload", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);
    await clickButton(wrapper, "客户端");

    expect(window.location.hash).toBe("#/settings/clients");
    expect(wrapper.find("#pane-clients").exists()).toBe(true);
    wrapper.unmount();

    const restored = await mountReady({
      admin_session: async () => ({
        username: "admin",
        mustChangePassword: false,
      }),
    });
    expect(restored.find("#pane-clients").exists()).toBe(true);
    restored.unmount();
  });

  it("starts on Home for a returning admin after the first login", async () => {
    localStorage.setItem(SETTINGS_SEEN_STORAGE_KEY, "1");

    const wrapper = await mountReady();
    await signIn(wrapper);

    expect(wrapper.text()).toContain("局域网中的服务器");
    expect(wrapper.text()).not.toContain("当前管理员");
    expect(document.body.textContent).toContain("管理员登录成功。");

    wrapper.unmount();
  });

  it("shows Settings on the first login and remembers it", async () => {
    const wrapper = await mountReady();
    await signIn(wrapper);

    expect(wrapper.find("#pane-shares").exists()).toBe(true);
    expect(localStorage.getItem(SETTINGS_SEEN_STORAGE_KEY)).toBe("1");

    wrapper.unmount();
  });

  it("keeps development reloads explicit and uses the same Tauri/Vite port", () => {
    const vite = readFileSync("vite.config.js", "utf8");
    const tauri = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
    expect(vite).toMatch(/hmr:\s*false/);
    for (const path of ["src-tauri", "dev_data", ".codegraph", ".cs", "dist"]) {
      expect(vite).toContain(`**/${path}/**`);
    }
    const port = Number(new URL(tauri.build.devUrl).port);
    expect(vite).toContain(`port: ${port}`);
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