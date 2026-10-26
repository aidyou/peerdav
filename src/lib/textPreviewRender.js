import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import cpp from "highlight.js/lib/languages/cpp";
import css from "highlight.js/lib/languages/css";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import kotlin from "highlight.js/lib/languages/kotlin";
import less from "highlight.js/lib/languages/less";
import php from "highlight.js/lib/languages/php";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import scss from "highlight.js/lib/languages/scss";
import sql from "highlight.js/lib/languages/sql";
import swift from "highlight.js/lib/languages/swift";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";
import MarkdownIt from "markdown-it";

import { fileExtension } from "./fileKinds";

const LANGUAGE_BY_EXTENSION = {
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  ts: "typescript",
  tsx: "typescript",
  vue: "xml",
  html: "xml",
  htm: "xml",
  xml: "xml",
  css: "css",
  scss: "scss",
  sass: "scss",
  less: "less",
  json: "json",
  jsonc: "json",
  json5: "json",
  ipynb: "json",
  avsc: "json",
  map: "json",
  php: "php",
  phtml: "php",
  php3: "php",
  php4: "php",
  php5: "php",
  php7: "php",
  phps: "php",
  py: "python",
  pyw: "python",
  go: "go",
  rs: "rust",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  cxx: "cpp",
  "c++": "cpp",
  hpp: "cpp",
  hh: "cpp",
  hxx: "cpp",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  swift: "swift",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "bash",
  sql: "sql",
  yaml: "yaml",
  yml: "yaml",
  toml: "ini",
  ini: "ini",
  conf: "ini",
  cfg: "ini",
  env: "ini",
  properties: "ini",
  cnf: "ini",
  rc: "ini",
  service: "ini",
  socket: "ini",
  timer: "ini",
  target: "ini",
  mount: "ini",
  htaccess: "ini",
  htpasswd: "ini",
  editorconfig: "ini",
  gitconfig: "ini",
  gitignore: "ini",
  gitattributes: "ini",
  dockerignore: "ini",
  npmrc: "ini",
  yarnrc: "ini",
  nvmrc: "ini",
  prettierrc: "ini",
  eslintrc: "ini",
  babelrc: "ini",
  stylelintrc: "ini",
  browserslistrc: "ini",
  bashrc: "bash",
  zshrc: "bash",
  profile: "bash",
  inputrc: "ini",
  netrc: "ini",
  wgetrc: "ini",
  vimrc: "ini",
  xsd: "xml",
  xsl: "xml",
  xslt: "xml",
  plist: "xml",
};

for (const [name, language] of Object.entries({
  bash,
  c,
  cpp,
  css,
  go,
  ini,
  java,
  javascript,
  json,
  kotlin,
  less,
  php,
  python,
  rust,
  scss,
  sql,
  swift,
  typescript,
  xml,
  yaml,
})) {
  hljs.registerLanguage(name, language);
}

const markdown = new MarkdownIt({
  html: false,
  linkify: true,
  typographer: false,
});

const renderLinkOpen =
  markdown.renderer.rules.link_open ??
  ((tokens, index, options, _env, self) =>
    self.renderToken(tokens, index, options));

markdown.renderer.rules.link_open = (tokens, index, options, env, self) => {
  tokens[index].attrSet("target", "_blank");
  tokens[index].attrSet("rel", "noopener noreferrer");
  return renderLinkOpen(tokens, index, options, env, self);
};

export function renderMarkdown(source) {
  return DOMPurify.sanitize(markdown.render(String(source ?? "")), {
    ADD_ATTR: ["target"],
  });
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** Returns escaped highlighted HTML and the language name used for it. */
export function highlightCode(source, entry) {
  const text = String(source ?? "");
  const language = LANGUAGE_BY_EXTENSION[fileExtension(entry?.name)];
  if (!language) {
    return { html: escapeHtml(text), language: "plaintext" };
  }
  try {
    return {
      html: hljs.highlight(text, { language, ignoreIllegals: true }).value,
      language,
    };
  } catch {
    return { html: escapeHtml(text), language: "plaintext" };
  }
}