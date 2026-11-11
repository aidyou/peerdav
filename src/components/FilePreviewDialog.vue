<script setup>
import { computed, nextTick, ref, shallowRef, watch } from "vue";

import FileGlyph from "./FileGlyph.vue";
import { t } from "../lib/i18n";
import { formatBytes } from "../lib/format";
import {
  MAX_PREVIEW_BYTES,
  MAX_PREVIEW_LINES,
  parseDelimited,
  previewFormat as detectPreviewFormat,
  tableDelimiter,
} from "../lib/textPreview";

// Shared by the LAN and cloud browsers so a preview looks and behaves the
// same no matter where the file lives.
const props = defineProps({
  open: { type: Boolean, default: false },
  target: { type: Object, default: null },
  preview: { type: Object, default: null },
  limit: { type: Object, default: null },
  downloadAllowed: { type: Boolean, default: true },
  busy: { type: Boolean, default: false },
});
const emit = defineEmits(["close", "download"]);

const dialogEl = ref(null);
const renderTools = shallowRef(null);
let renderToolsPromise = null;

const source = computed(() => {
  if (!props.preview || props.preview.encoding !== "base64") return "";
  return `data:${props.preview.mime};base64,${props.preview.data}`;
});
const isImage = computed(() => Boolean(props.preview?.mime?.startsWith("image/")));
const isAudio = computed(() => Boolean(props.preview?.mime?.startsWith("audio/")));
const isVideo = computed(() => Boolean(props.preview?.mime?.startsWith("video/")));
const formatKind = computed(() => detectPreviewFormat(props.target ?? {}));

const previewKind = computed(() => {
  if (!props.preview) return props.limit ? "unavailable" : "empty";
  if (isImage.value) return "image";
  if (isAudio.value) return "audio";
  if (isVideo.value) return "video";
  if (props.preview.encoding !== "utf8") return "binary";
  return formatKind.value;
});

const codePreview = computed(() =>
  previewKind.value === "code" && renderTools.value
    ? renderTools.value.highlightCode(props.preview?.data, props.target)
    : null,
);
const codeLineCount = computed(() =>
  previewKind.value === "code"
    ? Math.max(1, String(props.preview?.data ?? "").split("\n").length)
    : 0,
);
const markdownHtml = computed(() =>
  previewKind.value === "markdown" && renderTools.value
    ? renderTools.value.renderMarkdown(props.preview?.data)
    : "",
);
const tablePreview = computed(() =>
  previewKind.value === "table"
    ? parseDelimited(props.preview?.data, tableDelimiter(props.target))
    : null,
);
const meta = computed(() => {
  if (props.preview) {
    return `${props.preview.mime} · ${formatBytes(props.preview.size)}`;
  }
  if (props.target) return formatBytes(props.target.size);
  return "";
});
const limitText = computed(() => {
  if (!props.limit) return "";
  if (props.limit.code === "PEER_PREVIEW_TOO_MANY_LINES") {
    return t("peer.previewTooManyLines", {
      lines: props.limit.lines ?? "—",
      limit: props.limit.limit ?? MAX_PREVIEW_LINES,
    });
  }
  if (props.limit.code === "PEER_PREVIEW_TOO_LARGE") {
    return t("peer.previewTooLarge", {
      size: formatBytes(props.limit.size ?? 0),
      limit: formatBytes(props.limit.limit ?? MAX_PREVIEW_BYTES),
    });
  }
  return t("peer.previewUnavailable");
});

async function loadRenderTools() {
  if (renderTools.value) return renderTools.value;
  renderToolsPromise ??= import("../lib/textPreviewRender");
  try {
    renderTools.value = await renderToolsPromise;
  } catch (error) {
    renderToolsPromise = null;
    throw error;
  }
  return renderTools.value;
}

watch(
  () => [props.open, props.preview],
  async ([open, preview]) => {
    if (!open || !preview || preview.encoding !== "utf8") return;
    if (!["code", "markdown"].includes(formatKind.value)) return;
    try {
      await loadRenderTools();
    } catch {
      // Highlight/Markdown rendering is an enhancement; the preview still
      // reports the backend failure through the shared error handler.
    }
  },
  { immediate: true },
);

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    dialogEl.value?.focus();
  },
);
</script>

<template>
  <section
    v-if="open"
    ref="dialogEl"
    class="preview-dialog"
    role="dialog"
    aria-modal="true"
    :aria-label="target?.name ?? t('peer.preview')"
    tabindex="-1"
    @keydown.esc="emit('close')"
  >
    <header class="preview-dialog__head">
      <div class="preview-dialog__identity">
        <FileGlyph v-if="target" :entry="target" />
        <div>
          <strong>{{ target?.name ?? preview?.fileName }}</strong>
          <span>{{ meta }}</span>
        </div>
      </div>
      <div class="preview-dialog__actions">
        <span v-if="codePreview" class="preview-language">
          {{ codePreview.language }}
        </span>
        <el-button text @click="emit('close')">
          {{ t("common.close") }}
        </el-button>
      </div>
    </header>

    <div class="preview-dialog__body">
      <div v-if="previewKind === 'unavailable'" class="preview-unavailable">
        <FileGlyph v-if="target" :entry="target" size="xl" />
        <strong>{{ t("peer.previewUnavailable") }}</strong>
        <p>{{ limitText }}</p>
        <el-button v-if="downloadAllowed" type="primary" :disabled="busy" @click="emit('download')">
          {{ t("peer.download") }}
        </el-button>
      </div>

      <div v-else-if="previewKind === 'image'" class="preview-image-wrap">
        <img :src="source" :alt="t('peer.previewAlt')" class="preview-image-full" />
      </div>

      <div v-else-if="previewKind === 'audio'" class="preview-media-wrap">
        <audio :src="source" class="preview-audio" controls preload="metadata"></audio>
      </div>

      <div v-else-if="previewKind === 'video'" class="preview-media-wrap">
        <video :src="source" class="preview-video" controls playsinline preload="metadata"></video>
      </div>

      <div
        v-else-if="previewKind === 'markdown'"
        class="markdown-body"
        v-html="markdownHtml"
      ></div>

      <div v-else-if="previewKind === 'table'" class="preview-table-wrap">
        <p v-if="!tablePreview?.header.length && !tablePreview?.rows.length" class="preview-empty">
          {{ t("peer.previewEmpty") }}
        </p>
        <table v-else class="preview-table">
          <thead>
            <tr>
              <th class="preview-row-number" scope="col">#</th>
              <th v-for="column in tablePreview.columnCount" :key="column" scope="col">
                {{ tablePreview.header[column - 1] || t("peer.columnName", { number: column }) }}
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(row, rowIndex) in tablePreview.rows" :key="rowIndex">
              <th class="preview-row-number" scope="row">{{ rowIndex + 1 }}</th>
              <td v-for="column in tablePreview.columnCount" :key="column">
                {{ row[column - 1] ?? "" }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div v-else-if="previewKind === 'code'" class="preview-code-wrap">
        <pre class="preview-code-gutter" aria-hidden="true"><span
          v-for="line in codeLineCount"
          :key="line"
        >{{ line }}</span></pre>
        <pre class="preview-code"><code class="hljs" v-html="codePreview?.html"></code></pre>
      </div>

      <pre v-else-if="previewKind === 'text'" class="preview-text-full">{{ preview?.data }}</pre>

      <div v-else-if="previewKind === 'binary'" class="preview-empty-state">
        <p>{{ t("peer.binaryPreview") }}</p>
      </div>
    </div>
  </section>
</template>

<style scoped>
.preview-dialog {
  position: absolute;
  inset: 0;
  z-index: 40;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background: var(--app-surface);
  color: var(--app-text);
  outline: none;
}

.preview-dialog__head {
  flex: none;
  min-height: 54px;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 8px 14px;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-surface-2);
}

.preview-dialog__identity {
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 10px;
}

.preview-dialog__identity > div {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.preview-dialog__identity strong,
.preview-dialog__identity span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.preview-dialog__identity strong {
  font-size: 13px;
  color: var(--app-text);
}

.preview-dialog__identity span {
  font-size: 11.5px;
  color: var(--app-text-muted);
}

.preview-dialog__actions {
  flex: none;
  display: flex;
  align-items: center;
  gap: 10px;
}

.preview-language {
  padding: 3px 7px;
  border-radius: 6px;
  background: var(--pill-bg);
  color: var(--pill-fg);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 10.5px;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.preview-dialog__body {
  flex: 1;
  min-height: 0;
  overflow: auto;
  background: var(--app-surface);
}

.preview-image-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.preview-image-full {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  border-radius: 8px;
  border: 1px solid var(--app-border);
  background: var(--app-surface-2);
}

.preview-media-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.preview-audio {
  width: min(560px, 100%);
}

.preview-video {
  max-width: 100%;
  max-height: 100%;
  border-radius: 10px;
  background: #000;
  box-shadow: 0 12px 36px rgba(22, 32, 46, 0.2);
}

.markdown-body {
  max-width: 900px;
  margin: 0 auto;
  padding: 32px 40px 64px;
  color: var(--app-text-secondary);
  font-size: 14px;
  line-height: 1.75;
  overflow-wrap: anywhere;
}

.markdown-body :deep(h1),
.markdown-body :deep(h2),
.markdown-body :deep(h3),
.markdown-body :deep(h4) {
  margin: 1.5em 0 0.6em;
  color: var(--app-text);
  line-height: 1.3;
}

.markdown-body :deep(h1) {
  font-size: 1.8em;
  padding-bottom: 0.35em;
  border-bottom: 1px solid var(--app-border);
}

.markdown-body :deep(h2) {
  font-size: 1.4em;
  padding-bottom: 0.3em;
  border-bottom: 1px solid color-mix(in oklab, var(--app-border) 65%, transparent);
}

.markdown-body :deep(h3) {
  font-size: 1.18em;
}

.markdown-body :deep(p),
.markdown-body :deep(ul),
.markdown-body :deep(ol),
.markdown-body :deep(blockquote),
.markdown-body :deep(pre),
.markdown-body :deep(table) {
  margin: 0 0 1em;
}

.markdown-body :deep(ul),
.markdown-body :deep(ol) {
  padding-left: 1.6em;
}

.markdown-body :deep(li + li) {
  margin-top: 0.25em;
}

.markdown-body :deep(a) {
  color: var(--accent);
  text-decoration: none;
}

.markdown-body :deep(a:hover) {
  text-decoration: underline;
}

.markdown-body :deep(code) {
  padding: 0.18em 0.36em;
  border-radius: 5px;
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 0.88em;
  color: var(--app-text);
}

.markdown-body :deep(pre) {
  padding: 14px 16px;
  overflow: auto;
  border-radius: 8px;
  background: var(--app-surface-2);
  border: 1px solid var(--app-border);
}

.markdown-body :deep(pre code) {
  padding: 0;
  border: 0;
  background: transparent;
  font-size: 0.86em;
}

.markdown-body :deep(blockquote) {
  padding: 0.2em 0 0.2em 1em;
  border-left: 3px solid var(--app-border);
  color: var(--app-text-muted);
}

.markdown-body :deep(hr) {
  height: 1px;
  margin: 1.5em 0;
  border: 0;
  background: var(--app-border);
}

.markdown-body :deep(img) {
  max-width: 100%;
  border-radius: 8px;
}

.markdown-body :deep(table) {
  width: 100%;
  border-collapse: collapse;
  font-size: 0.92em;
}

.markdown-body :deep(th),
.markdown-body :deep(td) {
  padding: 7px 10px;
  border: 1px solid var(--app-border);
  text-align: left;
  vertical-align: top;
}

.markdown-body :deep(th) {
  background: var(--app-surface-2);
  color: var(--app-text);
}

.preview-table-wrap {
  min-width: 100%;
  padding-bottom: 24px;
}

.preview-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}

.preview-table th,
.preview-table td {
  max-width: 420px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--app-border);
  text-align: left;
  vertical-align: top;
  white-space: pre-wrap;
  word-break: break-word;
}

.preview-table thead th {
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--app-surface-2);
  color: var(--app-text);
  font-size: 11.5px;
  font-weight: 600;
  border-bottom-color: var(--app-border);
}

.preview-table tbody tr:nth-child(even) {
  background: color-mix(in oklab, var(--app-surface-2) 55%, transparent);
}

.preview-table .preview-row-number {
  width: 1%;
  padding-right: 10px;
  color: var(--app-text-muted);
  font-weight: 500;
  text-align: right;
  white-space: nowrap;
  user-select: none;
}

.preview-code-wrap {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  align-items: stretch;
  overflow: auto;
}

.preview-code-gutter {
  position: sticky;
  left: 0;
  z-index: 1;
  flex: none;
  box-sizing: border-box;
  margin: 0;
  padding: 20px 12px 48px 16px;
  border-right: 1px solid var(--app-border);
  background: var(--app-surface);
  color: var(--app-text-muted);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.65;
  text-align: right;
  user-select: none;
}

.preview-code-gutter span {
  display: block;
}

.preview-code {
  flex: 1 0 auto;
  min-width: max-content;
  min-height: 100%;
  box-sizing: border-box;
  margin: 0;
  padding: 20px 22px 48px;
  color: var(--app-text);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.65;
  tab-size: 2;
  white-space: pre;
}

.preview-code code {
  font-family: inherit;
}

.preview-text-full {
  min-height: 100%;
  box-sizing: border-box;
  margin: 0;
  padding: 24px 28px 48px;
  color: var(--app-text-secondary);
  font-family: "SFMono-Regular", ui-monospace, Menlo, Consolas, monospace;
  font-size: 12.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.preview-unavailable,
.preview-empty-state {
  min-height: 100%;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 32px;
  text-align: center;
}

.preview-unavailable strong {
  font-size: 16px;
  color: var(--app-text);
}

.preview-unavailable p,
.preview-empty-state p,
.preview-empty {
  max-width: 480px;
  margin: 0;
  color: var(--app-text-secondary);
  font-size: 13px;
  line-height: 1.6;
}

.preview-empty {
  padding: 32px;
  text-align: center;
}

.preview-code :deep(.hljs-comment),
.preview-code :deep(.hljs-quote),
.preview-code :deep(.hljs-meta) {
  color: var(--syntax-comment);
}

.preview-code :deep(.hljs-keyword),
.preview-code :deep(.hljs-literal),
.preview-code :deep(.hljs-selector-tag),
.preview-code :deep(.hljs-doctag) {
  color: var(--syntax-keyword);
}

.preview-code :deep(.hljs-string),
.preview-code :deep(.hljs-attr),
.preview-code :deep(.hljs-attribute),
.preview-code :deep(.hljs-addition),
.preview-code :deep(.hljs-template-tag) {
  color: var(--syntax-string);
}

.preview-code :deep(.hljs-number),
.preview-code :deep(.hljs-symbol),
.preview-code :deep(.hljs-regexp) {
  color: var(--syntax-number);
}

.preview-code :deep(.hljs-title),
.preview-code :deep(.hljs-section),
.preview-code :deep(.hljs-name),
.preview-code :deep(.hljs-selector-id),
.preview-code :deep(.hljs-selector-class) {
  color: var(--syntax-title);
}

.preview-code :deep(.hljs-built_in),
.preview-code :deep(.hljs-type) {
  color: var(--syntax-built-in);
}

.preview-code :deep(.hljs-variable),
.preview-code :deep(.hljs-template-variable),
.preview-code :deep(.hljs-params) {
  color: var(--syntax-variable);
}

.preview-code :deep(.hljs-deletion) {
  color: var(--syntax-deletion);
}

.preview-code :deep(.hljs-emphasis) {
  font-style: italic;
}

.preview-code :deep(.hljs-strong) {
  font-weight: 700;
}
</style>