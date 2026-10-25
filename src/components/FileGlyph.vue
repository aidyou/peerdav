<script setup>
import { computed } from "vue";

import { fileKind } from "../lib/fileKinds";

const props = defineProps({
  entry: { type: Object, required: true },
  size: { type: String, default: "md" },
});

const kind = computed(() => fileKind(props.entry));
</script>

<template>
  <span class="file-glyph" :class="[`file-glyph--${kind}`, `file-glyph--${size}`]">
    <svg v-if="kind === 'folder'" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="M3.5 7.2c0-.9.7-1.6 1.6-1.6h4l1.8 2h8c.9 0 1.6.7 1.6 1.6v8.5c0 .9-.7 1.6-1.6 1.6H5.1c-.9 0-1.6-.7-1.6-1.6z"
      />
    </svg>
    <svg v-else-if="kind === 'image'" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <circle cx="8.5" cy="9" r="1.4" />
      <path d="m5 17 4.4-4.6 3.1 3 2.1-2.2 4.4 3.8" />
    </svg>
    <svg v-else-if="kind === 'pdf'" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 3.5h7l5 5v12H6z" />
      <path d="M13 3.5v5h5" />
      <path d="M8.5 15.5h7M8.5 12.5h4" />
    </svg>
    <svg v-else-if="kind === 'word'" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 3.5h7l5 5v12H6z" />
      <path d="M13 3.5v5h5" />
      <path d="m8.5 12 1 5 1.5-3 1.5 3 1-5" />
    </svg>
    <svg
      v-else-if="kind === 'spreadsheet'"
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      <rect x="4.5" y="4" width="15" height="16" rx="1.5" />
      <path d="M4.5 9h15M4.5 14h15M9.5 4v16M14.5 4v16" />
    </svg>
    <svg
      v-else-if="kind === 'presentation'"
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      <rect x="3.5" y="4.5" width="17" height="12" rx="1.5" />
      <path d="M12 16.5V21M8 21h8" />
      <path d="m10.5 8 4 2.5-4 2.5z" />
    </svg>
    <svg v-else-if="kind === 'code'" viewBox="0 0 24 24" aria-hidden="true">
      <path d="m8.5 8-4 4 4 4M15.5 8l4 4-4 4M13.5 6l-3 12" />
    </svg>
    <svg v-else-if="kind === 'archive'" viewBox="0 0 24 24" aria-hidden="true">
      <rect x="4.5" y="4" width="15" height="16" rx="2" />
      <path d="M10.5 4v3M13.5 7v3M10.5 10v3M13.5 13v3M9.5 19h5" />
    </svg>
    <svg v-else-if="kind === 'text'" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 3.5h7l5 5v12H6z" />
      <path d="M13 3.5v5h5M9 12h6M9 15h6M9 18h4" />
    </svg>
    <svg v-else viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 3.5h7l5 5v12H6z" />
      <path d="M13 3.5v5h5" />
    </svg>
  </span>
</template>

<style scoped>
.file-glyph {
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--app-text-secondary);
}

.file-glyph svg {
  width: 100%;
  height: 100%;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.35;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.file-glyph--md {
  width: 24px;
  height: 24px;
}

.file-glyph--lg {
  width: 46px;
  height: 46px;
}

.file-glyph--xl {
  width: 76px;
  height: 76px;
}

.file-glyph--folder {
  color: var(--el-color-primary);
}

.file-glyph--image {
  color: color-mix(in oklab, var(--accent-green) 80%, var(--app-text));
}

.file-glyph--pdf {
  color: color-mix(in oklab, var(--accent-red) 75%, var(--app-text));
}

.file-glyph--word {
  color: #3f78c8;
}

.file-glyph--spreadsheet {
  color: #2f8f5b;
}

.file-glyph--presentation {
  color: #cc6a2f;
}

.file-glyph--code {
  color: color-mix(in oklab, var(--accent-purple) 72%, var(--app-text));
}

.file-glyph--archive {
  color: #9a7b42;
}

.file-glyph--text {
  color: color-mix(in oklab, var(--el-color-primary) 62%, var(--app-text-secondary));
}
</style>