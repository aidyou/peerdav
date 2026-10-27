// 出站访问申请的展示规则。状态本身由后端保存在内存里（`access_requests` 命令），
// 前端只负责按同一份数据渲染，因此页面重建、切换标签或 webview 重载都不会让
// 一台「已申请、等待批准」的服务器退回「可以申请」。

/**
 * 把 URL 拆成 host/port。无法解析时返回 null，调用方退回字符串比较。
 * @param {string | null | undefined} url
 * @returns {{ host: string, port: string } | null}
 */
export function urlEndpoint(url) {
  try {
    const parsed = new URL(String(url));
    const port = parsed.port || (parsed.protocol === "https:" ? "443" : "80");
    return { host: parsed.hostname.toLowerCase(), port };
  } catch {
    return null;
  }
}

/**
 * 局域网服务器的所有可访问地址：广播主地址 + 其它接口地址。
 * @param {{ url?: string, hosts?: string[], port?: number } | null} server
 * @returns {Array<{ host: string, port: string }>}
 */
export function serverEndpoints(server) {
  if (!server) return [];
  const endpoints = [];
  const primary = urlEndpoint(server.url);
  if (primary) endpoints.push(primary);
  const port = String(server.port ?? primary?.port ?? "");
  for (const host of server.hosts ?? []) {
    const normalized = String(host ?? "").trim().toLowerCase();
    if (!normalized || !port) continue;
    endpoints.push({ host: normalized, port });
  }
  return endpoints;
}

function sameEndpoint(left, right) {
  return Boolean(left && right && left.host === right.host && left.port === right.port);
}

/**
 * 找到这台服务器已保存的对端记录。先按地址精确匹配，再按 host:port 匹配：
 * 服务器重新广播时主地址可能换成另一块网卡，此时仍是同一台已连接的设备。
 * @param {Array<{ id: string, url: string }>} peers
 * @param {{ url?: string, hosts?: string[], port?: number }} server
 */
export function findSavedPeer(peers, server) {
  const fingerprint = server?.fingerprint ?? server?.serverId;
  if (fingerprint && peers.some((peer) => peer.fingerprint)) {
    return peers.find((peer) => peer.fingerprint === fingerprint) ?? null;
  }
  const exact = peers.find((peer) => peer.url === server?.url);
  if (exact) return exact;
  const endpoints = serverEndpoints(server);
  return (
    peers.find((peer) => {
      const endpoint = urlEndpoint(peer.url);
      return endpoints.some((candidate) => sameEndpoint(candidate, endpoint));
    }) ?? null
  );
}

/**
 * 找到这台服务器尚未完成的申请（等待批准 / 已取消 / 已过期）。
 * @param {Array<{ url: string, status: string }>} entries
 * @param {{ url?: string, hosts?: string[], port?: number }} server
 */
export function findAccessRequest(entries, server) {
  const fingerprint = server?.fingerprint ?? server?.serverId;
  const identified = fingerprint && entries.find((entry) => entry.fingerprint === fingerprint);
  if (identified) return identified;
  const candidates = fingerprint ? entries.filter((entry) => !entry.fingerprint) : entries;
  const exact = candidates.find((entry) => entry.url === server?.url);
  if (exact) return exact;
  const endpoints = serverEndpoints(server);
  return (
    candidates.find((entry) => {
      const endpoint = urlEndpoint(entry.url);
      return endpoints.some((candidate) => sameEndpoint(candidate, endpoint));
    }) ?? null
  );
}

/**
 * 一行申请状态的展示含义。`status` 与后端 `OutboundAccessStatus` 同名，
 * 便于两个视图共用同一套标签配色。
 * @param {{ status: string, message?: string | null } | null | undefined} entry
 */
export function accessRowState(entry) {
  switch (entry?.status) {
    case "waiting":
      return {
        status: "pending",
        canRequest: false,
        canCancel: true,
        messageKey: "home.addWaiting",
        message: "",
      };
    case "accessible":
      return {
        status: "unrequested",
        canRequest: false,
        canCancel: false,
        messageKey: "home.addAlreadyAccessible",
        message: "",
      };
    case "canceled":
      return {
        status: "canceled",
        canRequest: true,
        canCancel: false,
        messageKey: "home.requestCanceled",
        message: "",
      };
    case "expired":
    case "failed":
      return {
        status: "error",
        canRequest: true,
        canCancel: false,
        messageKey: "",
        message: entry.message ?? "",
      };
    default:
      return {
        status: "unrequested",
        canRequest: true,
        canCancel: false,
        messageKey: "",
        message: "",
      };
  }
}