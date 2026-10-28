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
const WORD_EXTENSIONS = new Set(["doc", "docs", "docx", "docm", "dot", "dotx", "odt", "rtf", "pages"]);
const WORD_TYPES = new Set([
  "application/msword",
  "application/rtf",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.oasis.opendocument.text",
]);
const SHEET_EXTENSIONS = new Set([
  "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "csv", "tsv", "ods", "numbers",
]);
const SHEET_TYPES = new Set([
  "application/vnd.ms-excel",
  "text/csv",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.oasis.opendocument.spreadsheet",
]);
const SLIDE_EXTENSIONS = new Set(["ppt", "pptx", "pptm", "pps", "ppsx", "potx", "odp", "key"]);
const SLIDE_TYPES = new Set([
  "application/vnd.ms-powerpoint",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/vnd.oasis.opendocument.presentation",
]);
const CODE_EXTENSIONS = new Set([
  "js", "mjs", "cjs", "jsx", "ts", "tsx", "tx", "vue", "svelte", "astro",
  "html", "htm", "shtml", "xhtml", "mts", "cts", "php8", "h++", "css", "scss", "sass", "less", "json", "jsonc", "json5",
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
  "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "bz", "tbz2", "txz", "lz", "lzma", "br", "cab", "jar", "war", "ear",
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

const DATABASE_EXTENSIONS = new Set(["db", "sqlite", "sqlite3", "mdb", "accdb", "dbf"]);
const FONT_EXTENSIONS = new Set(["ttf", "otf", "woff", "woff2", "eot"]);
const PACKAGE_EXTENSIONS = new Set(["exe", "msi", "msix", "appx", "dmg", "pkg", "deb", "rpm", "apk", "appimage", "iso"]);

// Presentation metadata is separate from preview capabilities and type labels.
const CODE_BADGES = [
  ["GO", "go", ["go"]], ["RS", "rust", ["rs"]],
  ["PHP", "php", ["php", "phtml", "php3", "php4", "php5", "php7", "php8", "phps"]],
  ["JS", "javascript", ["js", "mjs", "cjs", "jsx"]],
  ["TS", "typescript", ["ts", "tsx", "mts", "cts"]],
  ["CSS", "css", ["css", "scss", "sass", "less"]],
  ["HTML", "html", ["html", "htm", "shtml", "xhtml"]],
  ["LUA", "lua", ["lua"]], ["JAVA", "java", ["java", "class", "jar"]],
  ["C", "c", ["c", "h"]], ["C++", "cpp", ["cc", "cpp", "cxx", "c++", "hpp", "hh", "hxx", "h++"]],
  ["C#", "csharp", ["cs", "csx"]], ["XML", "xml", ["xml", "xsd", "xsl", "xslt", "plist"]],
  ["PY", "python", ["py", "pyw", "ipynb"]], ["RB", "ruby", ["rb"]],
  ["VUE", "vue", ["vue"]], ["SV", "svelte", ["svelte"]],
  ["KT", "kotlin", ["kt", "kts"]], ["SW", "swift", ["swift"]],
  ["DART", "dart", ["dart"]], ["SH", "shell", ["sh", "bash", "zsh", "fish", "ps1", "psm1", "bat", "cmd"]],
  ["SQL", "sql", ["sql"]], ["JSON", "json", ["json", "jsonc", "json5"]],
  ["YML", "yaml", ["yaml", "yml"]], ["TOML", "toml", ["toml"]],
];
const BADGE_BY_EXTENSION = new Map(CODE_BADGES.flatMap(([label, tone, extensions]) =>
  extensions.map((extension) => [extension, { label, tone }])));

export function fileIconBadge(entry) {
  if (entry?.isDir || !["code", "archive"].includes(fileKind(entry))) return null;
  return BADGE_BY_EXTENSION.get(fileExtension(entry?.name)) ?? null;
}

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
  const type = String(entry?.contentType ?? "").split(";")[0].trim().toLowerCase();
  const name = String(entry?.name ?? "").toLowerCase();
  const extension = fileExtension(entry?.name);
  if (type.startsWith("image/") || IMAGE_EXTENSIONS.has(extension)) {
    return "image";
  }
  if (AUDIO_EXTENSIONS.has(extension) || type.startsWith("audio/")) return "audio";
  // .ts is TypeScript in a code workspace, even when MIME guesses MPEG-TS.
  if (extension !== "ts" && (VIDEO_EXTENSIONS.has(extension) || type.startsWith("video/"))) return "video";
  if (DATABASE_EXTENSIONS.has(extension)) return "database";
  if (FONT_EXTENSIONS.has(extension) || type.startsWith("font/")) return "font";
  if (PACKAGE_EXTENSIONS.has(extension)) return "package";
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
    audio: "peer.audio",
    video: "peer.video",
    database: "peer.database",
    font: "peer.font",
    package: "peer.package",
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