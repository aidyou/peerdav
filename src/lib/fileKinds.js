const IMAGE_EXTENSIONS = new Set([
  "png", "jpg", "jpeg", "jfif", "gif", "webp", "svg", "bmp", "ico", "avif",
  "heic", "tif", "tiff",
]);
const AUDIO_EXTENSIONS = new Set([
  "mp3", "wav", "ogg", "oga", "m4a", "aac", "flac", "opus", "weba", "aif",
  "aiff", "wma",
]);
const VIDEO_EXTENSIONS = new Set([
  "mp4", "m4v", "webm", "ogv", "mov", "mkv", "avi", "wmv", "flv", "3gp",
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
  "js", "mjs", "cjs", "jsx", "ts", "tsx", "tx", "vue", "svelte", "astro",
  "html", "htm", "css", "scss", "sass", "less", "json", "jsonc", "json5",
  "php", "py", "go", "rs", "c", "h", "cc", "cpp", "cxx", "c++", "hpp",
  "hh", "java", "kt", "kts", "swift", "sh", "bash", "zsh", "fish", "sql",
  "xml", "yaml", "yml", "toml", "ini", "conf", "cfg", "env", "properties",
  "cs", "csx", "fs", "fsx", "vb", "dart", "lua", "pl", "pm", "rb", "scala",
  "clj", "cljs", "cljc", "edn", "ex", "exs", "erl", "hrl", "hs", "lhs",
  "r", "jl", "nim", "zig", "sol", "groovy", "gradle", "cmake", "mk",
  "make", "asm", "s", "v", "sv", "vhd", "vhdl", "tcl", "awk", "sed",
  "bat", "cmd", "ps1", "psm1", "pas", "f90", "f95", "m", "mm", "elm",
  "purs", "re", "rei", "ml", "mli", "graphql", "gql", "proto", "thrift",
  "htaccess", "htpasswd", "gitignore", "gitattributes", "gitconfig",
  "dockerignore", "editorconfig", "npmrc", "yarnrc", "nvmrc", "prettierrc",
  "eslintrc", "babelrc", "stylelintrc", "browserslistrc", "phtml", "php3",
  "php4", "php5", "php7", "phps", "pyw", "hxx", "xsd", "xsl", "xslt",
  "plist", "ipynb", "avsc", "map", "cnf", "rc", "rules", "service", "socket",
  "timer", "target", "mount", "lock", "pid", "bashrc", "zshrc", "profile",
  "vimrc", "inputrc", "netrc", "wgetrc",
]);
const CODE_TYPES = new Set([
  "application/javascript",
  "application/json",
  "application/xml",
  "application/x-httpd-php",
  "application/x-sh",
  "application/x-yaml",
  "application/yaml",
  "application/toml",
  "application/x-toml",
  "application/typescript",
  "application/x-typescript",
  "text/x-python",
  "text/x-rust",
  "text/x-go",
  "text/x-markdown",
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
  "txt", "md", "markdown", "mdown", "mkd", "mdx", "log", "rst", "tex",
  "adoc", "asciidoc", "org", "text", "1st", "nfo", "srt", "vtt", "desktop",
]);
const TEXT_FILENAMES = new Set([
  "dockerfile", "makefile", "gemfile", "rakefile", "procfile", "jenkinsfile",
  "vagrantfile", "license", "licence", "readme", "changelog", "authors",
  "notice", "copying", "todo",
]);
const READABLE_TEXT_EXTENSIONS = new Set([
  ...TEXT_EXTENSIONS,
  ...CODE_EXTENSIONS,
  "csv",
  "tsv",
]);

export function fileExtension(name) {
  const lower = String(name ?? "").toLowerCase();
  return lower.includes(".") ? lower.slice(lower.lastIndexOf(".") + 1) : "";
}

function isEnvFile(name) {
  const lower = String(name ?? "").toLowerCase();
  return lower === ".env" || lower.startsWith(".env.");
}

/** Returns a stable icon/type category for a peer entry. */
export function fileKind(entry) {
  if (entry?.isDir) return "folder";
  const type = String(entry?.contentType ?? "").toLowerCase();
  const name = String(entry?.name ?? "").toLowerCase();
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
  if (
    type.startsWith("text/") ||
    TEXT_EXTENSIONS.has(extension) ||
    TEXT_FILENAMES.has(name) ||
    isEnvFile(name)
  ) {
    return "text";
  }
  return "file";
}

/** True when the entry can be previewed or externally edited as plain text. */
export function isReadableText(entry) {
  if (!entry || entry.isDir) return false;
  if (TEXT_FILENAMES.has(String(entry.name ?? "").toLowerCase())) return true;
  if (isEnvFile(entry.name)) return true;
  return READABLE_TEXT_EXTENSIONS.has(fileExtension(entry.name));
}

/**
 * True when the entry has a known inline-preview format. The decision uses the
 * file suffix, not the server-reported MIME type.
 */
export function canPreview(entry) {
  if (!entry || entry.isDir) return false;
  if (isReadableText(entry)) return true;
  const extension = fileExtension(entry.name);
  return (
    IMAGE_EXTENSIONS.has(extension) ||
    AUDIO_EXTENSIONS.has(extension) ||
    VIDEO_EXTENSIONS.has(extension)
  );
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