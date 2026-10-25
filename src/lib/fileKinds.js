const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif", "heic",
]);
const WORD_EXTENSIONS = new Set(["doc", "docs", "docx", "odt", "rtf", "pages"]);
const WORD_TYPES = new Set([
  "application/msword",
  "application/rtf",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.oasis.opendocument.text",
]);
const SHEET_EXTENSIONS = new Set([
  "xls", "xlsx", "csv", "ods", "numbers",
]);
const SHEET_TYPES = new Set([
  "application/vnd.ms-excel",
  "text/csv",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.oasis.opendocument.spreadsheet",
]);
const SLIDE_EXTENSIONS = new Set(["ppt", "pptx", "odp", "key"]);
const SLIDE_TYPES = new Set([
  "application/vnd.ms-powerpoint",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.oasis.opendocument.presentation",
]);
const CODE_EXTENSIONS = new Set([
  "js", "mjs", "cjs", "jsx", "ts", "tsx", "tx", "vue", "html", "htm", "css",
  "scss", "sass", "less", "json", "jsonc", "php", "py", "go", "rs", "c",
  "h", "cc", "cpp", "cxx", "c++", "hpp", "hh", "java", "kt", "kts", "swift", "sh",
  "bash", "zsh", "fish", "sql", "xml", "yaml", "yml", "toml", "ini",
]);
const CODE_TYPES = new Set([
  "application/javascript",
  "application/json",
  "application/xml",
  "application/x-httpd-php",
  "application/x-sh",
  "application/x-yaml",
]);
const ARCHIVE_EXTENSIONS = new Set([
  "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst",
]);
const ARCHIVE_TYPES = new Set([
  "application/zip",
  "application/x-7z-compressed",
  "application/x-rar-compressed",
  "application/gzip",
  "application/x-tar",
]);
const TEXT_EXTENSIONS = new Set([
  "txt", "md", "markdown", "log", "rst", "tex", "conf", "cfg", "env",
]);

export function fileExtension(name) {
  const lower = String(name ?? "").toLowerCase();
  return lower.includes(".") ? lower.slice(lower.lastIndexOf(".") + 1) : "";
}

/** Returns a stable icon/type category for a peer entry. */
export function fileKind(entry) {
  if (entry?.isDir) return "folder";
  const type = String(entry?.contentType ?? "").toLowerCase();
  const extension = fileExtension(entry?.name);
  if (type.startsWith("image/") || IMAGE_EXTENSIONS.has(extension)) {
    return "image";
  }
  if (type === "application/pdf" || extension === "pdf") return "pdf";
  if (WORD_TYPES.has(type) || WORD_EXTENSIONS.has(extension)) return "word";
  if (SHEET_TYPES.has(type) || SHEET_EXTENSIONS.has(extension)) {
    return "spreadsheet";
  }
  if (SLIDE_TYPES.has(type) || SLIDE_EXTENSIONS.has(extension)) {
    return "presentation";
  }
  if (CODE_TYPES.has(type) || CODE_EXTENSIONS.has(extension)) return "code";
  if (ARCHIVE_TYPES.has(type) || ARCHIVE_EXTENSIONS.has(extension)) {
    return "archive";
  }
  if (type.startsWith("text/") || TEXT_EXTENSIONS.has(extension)) return "text";
  return "file";
}

export function fileKindKey(kind) {
  return {
    folder: "peer.folder",
    image: "peer.image",
    pdf: "peer.pdf",
    word: "peer.word",
    spreadsheet: "peer.spreadsheet",
    presentation: "peer.presentation",
    code: "peer.code",
    archive: "peer.archive",
    text: "peer.textFile",
    file: "peer.file",
  }[kind] ?? "peer.file";
}