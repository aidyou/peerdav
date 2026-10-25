<script setup>
import { onBeforeUnmount, onMounted, ref } from "vue";

/**
 * Icon-triggered dropdown used in the custom title bar. It renders its menu
 * inline (no teleport) so it stays reachable to unit tests and respects the
 * window drag region. Each option is a radio item; the active value is marked
 * and the menu closes on selection, outside click or Escape.
 */
defineProps({
  name: { type: String, required: true },
  ariaLabel: { type: String, required: true },
  modelValue: { type: String, required: true },
  // [{ value, label, lang? }]
  items: { type: Array, required: true },
});

const emit = defineEmits(["update:modelValue"]);

const open = ref(false);
const root = ref(null);

function toggle() {
  open.value = !open.value;
}

function close() {
  open.value = false;
}

function select(value) {
  emit("update:modelValue", value);
  close();
}

function onPointerDown(event) {
  if (root.value && !root.value.contains(event.target)) close();
}

function onKeydown(event) {
  if (event.key === "Escape") close();
}

onMounted(() => {
  document.addEventListener("mousedown", onPointerDown);
  document.addEventListener("keydown", onKeydown);
});

onBeforeUnmount(() => {
  document.removeEventListener("mousedown", onPointerDown);
  document.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <div ref="root" class="titlebar-menu" :data-menu="name">
    <el-tooltip
      :content="ariaLabel"
      placement="bottom"
      :show-after="200"
      :hide-after="0"
      :enterable="false"
    >
      <button
        type="button"
        class="titlebar-menu__trigger"
        :class="{ 'is-open': open }"
        :aria-label="ariaLabel"
        :aria-expanded="open ? 'true' : 'false'"
        aria-haspopup="menu"
        @click="toggle"
      >
        <slot name="icon" />
      </button>
    </el-tooltip>
    <ul v-if="open" class="titlebar-menu__list" role="menu">
      <li
        v-for="item in items"
        :key="item.value"
        class="titlebar-menu__item"
        :class="{ 'is-active': item.value === modelValue }"
        role="menuitemradio"
        :aria-checked="item.value === modelValue ? 'true' : 'false'"
        :data-value="item.value"
        :lang="item.lang"
        @click="select(item.value)"
      >
        <span class="titlebar-menu__label">{{ item.label }}</span>
        <svg
          v-if="item.value === modelValue"
          class="titlebar-menu__check"
          viewBox="0 0 16 16"
          aria-hidden="true"
        >
          <path d="M3 8.5l3 3 7-7" />
        </svg>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.titlebar-menu {
  position: relative;
  -webkit-app-region: no-drag;
}

.titlebar-menu__trigger {
  width: 30px;
  height: 30px;
  border: 0;
  border-radius: 8px;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  color: var(--app-text-muted);
  background: transparent;
  -webkit-app-region: no-drag;
  transition:
    background 0.15s ease,
    color 0.15s ease;
}

.titlebar-menu__trigger:hover,
.titlebar-menu__trigger.is-open {
  color: var(--app-text);
  background: var(--pill-bg);
}

.titlebar-menu__trigger:focus-visible {
  outline: 2px solid var(--el-color-primary);
  outline-offset: 1px;
}

.titlebar-menu__trigger :deep(svg) {
  width: 16px;
  height: 16px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.titlebar-menu__list {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 50;
  min-width: 150px;
  margin: 0;
  padding: 6px;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 2px;
  background: var(--app-surface);
  border: 1px solid var(--app-border);
  border-radius: 12px;
  box-shadow: 0 12px 32px rgba(22, 32, 46, 0.18);
}

.titlebar-menu__item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 7px 10px;
  border-radius: 8px;
  font-size: 13px;
  cursor: pointer;
  color: var(--app-text);
}

.titlebar-menu__item:hover {
  background: var(--pill-bg);
}

.titlebar-menu__item.is-active {
  color: var(--el-color-primary);
  font-weight: 600;
}

.titlebar-menu__check {
  width: 14px;
  height: 14px;
  flex: none;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}

/* Half-filled "auto" glyph for the system theme option. */
.titlebar-menu__trigger :deep(.titlebar-icon-fill) {
  fill: currentColor;
  stroke: none;
}
</style>
