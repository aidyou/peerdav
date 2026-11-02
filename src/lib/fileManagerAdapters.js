// Pure, provider-neutral selection decisions. Callers retain the only selection
// state and all preview/navigation side effects. Raw identity is never rewritten.
export function selectionForClick({ entries, selection, anchor, entry, identityField, additive, ranged }) {
  const id = (item) => item[identityField];
  if (ranged && anchor != null) {
    const from = entries.findIndex((item) => id(item) === anchor);
    const to = entries.findIndex((item) => id(item) === id(entry));
    if (from !== -1 && to !== -1) {
      return { selection: entries.slice(Math.min(from, to), Math.max(from, to) + 1), anchor };
    }
  }
  if (!additive) return null; // Provider's selectSingle owns gallery preview.
  const added = !selection.some((item) => id(item) === id(entry));
  const ids = new Set(selection.map(id));
  if (added) ids.add(id(entry));
  else ids.delete(id(entry));
  return { selection: entries.filter((item) => ids.has(id(item))), anchor: added ? id(entry) : anchor };
}
