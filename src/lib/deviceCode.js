// 设备身份展示：机器码是「这台设备是谁」的唯一标志，IP 只是当时拿到的地址，
// 换个 DHCP 租约就会变。列表里短码足够比对，完整值在详情里看。

/**
 * 机器码短码：前 8 位 + 后 4 位。长度不足时原样返回。
 * @param {string | null | undefined} code
 * @returns {string}
 */
export function shortMachineCode(code) {
  const trimmed = String(code ?? "").trim();
  if (!trimmed) {
    return "";
  }
  if (trimmed.length <= 16) {
    return trimmed;
  }
  return `${trimmed.slice(0, 8)}…${trimmed.slice(-4)}`;
}

/**
 * 待审批申请的身份说明：机器码优先，缺失时说明只能按地址识别（例如普通
 * WebDAV 客户端或老版本 PeerDav 不会上报机器码）。
 * @param {{machineId?: string | null}} request
 * @returns {{ short: string, trusted: boolean }}
 */
export function pendingIdentity(request) {
  const short = shortMachineCode(request?.machineId);
  return { short, trusted: short !== "" };
}