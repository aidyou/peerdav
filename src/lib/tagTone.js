/// 列表里的数量与状态统一用 el-tag 表达。一个颜色只表示一个含义：
/// 成功/就绪 = success（绿），失败/不可用 = danger（红），等待用户处理 = warning（琥珀），
/// 进行中与中性事实 = info（灰蓝）。强调色不参与状态，它只表示「可操作」，
/// 否则同一屏里品牌色既当按钮又当状态，读起来就是四五种颜色在抢视线。
const STATUS_TONE = {
  approved: "success",
  authorized: "success",
  connected: "success",
  online: "success",
  ready: "success",
  done: "success",

  // 只有需要用户动手才有结果的等待，才用琥珀色提醒。
  pending: "warning",
  requesting: "warning",

  // 自动推进的中间态与没有状态可言的空档，一律用中性灰蓝。
  waiting: "info",
  queued: "info",
  downloading: "info",
  uploading: "info",
  unrequested: "info",
  canceled: "info",
  skipped: "info",
  unknown: "info",

  offline: "danger",
  error: "danger",
  failed: "danger",
  denied: "danger",
};

/**
 * @param {string} status 内部状态标识，不是面向用户的文案
 * @returns {"success" | "danger" | "warning" | "info"} el-tag 的 type
 */
export function statusTagTone(status) {
  return STATUS_TONE[status] ?? "info";
}
