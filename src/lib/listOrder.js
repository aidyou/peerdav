// Identity order is independent of status, network address and backend iteration order.
export function stableList(items, identity) {
  return [...(Array.isArray(items) ? items : [])].sort((a, b) => {
    const left = String(identity(a) ?? "");
    const right = String(identity(b) ?? "");
    return left < right ? -1 : left > right ? 1 : 0;
  });
}
