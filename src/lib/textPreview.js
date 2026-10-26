import { fileExtension, fileKind } from "./fileKinds";

/** Keep these limits aligned with `MAX_PREVIEW_*` in `src-tauri/src/types.rs`. */
export const MAX_PREVIEW_BYTES = 10 * 1024 * 1024;
export const MAX_PREVIEW_LINES = 5000;

const MARKDOWN_EXTENSIONS = new Set(["md", "markdown", "mdown", "mkd", "mdx"]);
const MARKDOWN_TYPES = new Set(["text/markdown", "text/x-markdown"]);
const TABLE_EXTENSIONS = new Set(["csv", "tsv"]);
const TABLE_TYPES = new Set(["text/csv", "text/tab-separated-values"]);

function normalizedType(entry) {
  return String(entry?.contentType ?? "")
    .toLowerCase()
    .split(";")[0]
    .trim();
}

/** Returns the renderer that should be used for an entry's text preview. */
export function previewFormat(entry) {
  const extension = fileExtension(entry?.name);
  const type = normalizedType(entry);
  if (MARKDOWN_EXTENSIONS.has(extension) || MARKDOWN_TYPES.has(type)) {
    return "markdown";
  }
  if (TABLE_EXTENSIONS.has(extension) || TABLE_TYPES.has(type)) {
    return "table";
  }
  if (fileKind(entry) === "code") return "code";
  return "text";
}

/** CSV unless the extension or content type explicitly identifies TSV. */
export function tableDelimiter(entry) {
  return fileExtension(entry?.name) === "tsv" ||
    normalizedType(entry) === "text/tab-separated-values"
    ? "\t"
    : ",";
}

/**
 * Parses RFC 4180-style delimited text, including quoted separators and
 * newlines. The backend enforces the line-count limit before this runs.
 */
export function parseDelimited(source, delimiter = ",") {
  const input = String(source ?? "").replace(/^\uFEFF/, "");
  const rows = [];
  let row = [];
  let field = "";
  let quoted = false;

  for (let index = 0; index < input.length; index += 1) {
    const char = input[index];
    if (quoted) {
      if (char === '"') {
        if (input[index + 1] === '"') {
          field += '"';
          index += 1;
        } else {
          quoted = false;
        }
      } else {
        field += char;
      }
      continue;
    }

    if (char === '"' && field === "") {
      quoted = true;
    } else if (char === delimiter) {
      row.push(field);
      field = "";
    } else if (char === "\n" || char === "\r") {
      if (char === "\r" && input[index + 1] === "\n") index += 1;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else {
      field += char;
    }
  }

  if (field !== "" || row.length > 0) {
    row.push(field);
    rows.push(row);
  }

  const columnCount = rows.reduce(
    (count, cells) => Math.max(count, cells.length),
    0,
  );
  return {
    header: rows[0] ?? [],
    rows: rows.slice(1),
    columnCount,
  };
}