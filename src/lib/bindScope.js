// 服务绑定范围：首选「广域网 / 局域网 / 本机」三项，后面列出其余本机网卡地址
// （VPN/tun、多网卡都在这），再兜底显示历史上手填过但已不在候选里的地址，
// 避免下拉框悄悄改掉用户已有的配置。下拉框本身允许输入，所以公网地址或
// IPv6 也能直接填。
export const BIND_WAN = "0.0.0.0";
export const BIND_LOCALHOST = "127.0.0.1";

/**
 * @param {{lanAddresses?: string[], current?: string, labels: {wan: string, lan: string, local: string, other: string, custom: string}}} input
 * @returns {Array<{value: string, scope: string, address: string, text: string}>}
 */
export function buildBindOptions({ lanAddresses = [], current = "", labels }) {
  const presets = new Set([BIND_WAN, BIND_LOCALHOST]);
  const interfaces = [];
  for (const address of lanAddresses) {
    const value = String(address ?? "").trim();
    // 没有可用网卡地址（例如未联网的机器）时不给出「局域网」，选了也绑不上。
    if (!value || presets.has(value) || interfaces.includes(value)) {
      continue;
    }
    interfaces.push(value);
  }

  const options = [
    { value: BIND_WAN, scope: labels.wan, address: BIND_WAN },
  ];

  // 第一项由后端按默认路由排好，所以「局域网」就是真正出网的那张网卡；
  // 其余网卡跟在本机之后，需要时靠输入过滤，不必在长列表里翻。
  if (interfaces.length > 0) {
    options.push({
      value: interfaces[0],
      scope: labels.lan,
      address: interfaces[0],
    });
  }
  options.push({
    value: BIND_LOCALHOST,
    scope: labels.local,
    address: BIND_LOCALHOST,
  });
  for (const address of interfaces.slice(1)) {
    options.push({ value: address, scope: labels.other, address });
  }

  const saved = current.trim();
  if (saved && !options.some((option) => option.value === saved)) {
    options.push({ value: saved, scope: labels.custom, address: saved });
  }

  return options.map((option) => ({
    ...option,
    text: `${option.scope} ${option.address}`,
  }));
}
