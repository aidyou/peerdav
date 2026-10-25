<script setup>
import { t } from "../lib/i18n";

defineProps({
  peers: { type: Array, required: true },
  activePeerId: { type: String, default: "" },
  activeView: { type: String, default: "home" },
});

const emit = defineEmits(["open-home", "open-peer", "open-settings"]);

function initials(peer) {
  const label = (peer?.label ?? "").trim();
  return label ? [...label].slice(0, 2).join("").toUpperCase() : "??";
}
</script>

<template>
  <aside class="rail">
    <el-tooltip
      :content="t('home.title')"
      placement="right"
      :show-after="200"
      :hide-after="0"
      :enterable="false"
    >
      <button
        type="button"
        class="rail-action rail-home"
        :class="{ 'is-active': activeView === 'home' }"
        :aria-label="t('home.title')"
        :aria-current="activeView === 'home' ? 'page' : undefined"
        @click="emit('open-home')"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 10.5 12 4l8 6.5" />
          <path d="M6.5 9.5V20h11V9.5" />
          <path d="M10 20v-5h4v5" />
        </svg>
      </button>
    </el-tooltip>
    <div class="rail-servers">
      <el-tooltip
        v-for="peer in peers"
        :key="peer.id"
        :content="peer.label"
        placement="right"
        :show-after="200"
        :hide-after="0"
        :enterable="false"
      >
        <button
          type="button"
          class="rail-avatar"
          :class="{
            'is-active': activeView === 'peer' && activePeerId === peer.id,
          }"
          :aria-label="peer.label"
          @click="emit('open-peer', peer)"
        >
          {{ initials(peer) }}
        </button>
      </el-tooltip>
    </div>
    <el-tooltip
      :content="t('settings.open')"
      placement="right"
      :show-after="200"
      :hide-after="0"
      :enterable="false"
    >
      <button
        type="button"
        class="rail-action"
        :class="{ 'is-active': activeView === 'settings' }"
        :aria-label="t('settings.open')"
        :aria-current="activeView === 'settings' ? 'page' : undefined"
        @click="emit('open-settings')"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path
            d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.03 1.56V21a2 2 0 1 1-4 0v-.09a1.7 1.7 0 0 0-1.11-1.56 1.7 1.7 0 0 0-1.88.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.03H3a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.6 8.86a1.7 1.7 0 0 0-.34-1.88l-.06-.06A2 2 0 1 1 7.03 4.09l.06.06a1.7 1.7 0 0 0 1.88.34H9a1.7 1.7 0 0 0 1.03-1.56V3a2 2 0 1 1 4 0v.09a1.7 1.7 0 0 0 1.03 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.7 1.7 0 0 0-.34 1.88V9a1.7 1.7 0 0 0 1.56 1.03H21a2 2 0 1 1 0 4h-.09A1.7 1.7 0 0 0 19.4 15z"
          />
        </svg>
      </button>
    </el-tooltip>
  </aside>
</template>

<style scoped>
.rail {
  width: 64px;
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 10px 0;
  box-sizing: border-box;
  background: var(--titlebar-bg);
  border-right: 1px solid var(--titlebar-border);
}

.rail-servers {
  flex: 1;
  min-height: 0;
  width: 100%;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}

.rail-avatar,
.rail-action {
  width: 40px;
  height: 40px;
  flex: none;
  border: 1px solid transparent;
  border-radius: 12px;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  -webkit-app-region: no-drag;
  color: var(--app-text);
  background: var(--pill-bg);
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.02em;
  transition:
    background 0.15s ease,
    color 0.15s ease,
    border-color 0.15s ease;
}

.rail-action {
  color: var(--app-text-muted);
  background: transparent;
}

.rail-home {
  margin-bottom: 2px;
}

.rail-avatar:hover,
.rail-action:hover {
  background: var(--app-surface-3);
}

.rail-avatar.is-active {
  border-color: var(--el-color-primary);
  color: var(--el-color-primary);
  background: color-mix(in oklab, var(--el-color-primary) 14%, transparent);
}

.rail-action.is-active {
  color: var(--app-text);
  background: var(--pill-bg);
}

.rail-action svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

@media (max-width: 560px) {
  .rail {
    width: 56px;
  }

  .rail-avatar,
  .rail-action {
    width: 36px;
    height: 36px;
    border-radius: 10px;
  }
}
</style>