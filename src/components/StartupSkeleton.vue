<script setup>
import { computed } from "vue";

import { t } from "../lib/i18n";
import ListSkeleton from "./ListSkeleton.vue";

/**
 * 重载过渡期的内容占位。
 *
 * 这块骨架必须和真实页面共用同一套几何：内容区内边距由 App.vue 的
 * .workspace-content 负责，这里只还原首页的区块节奏（30px 区块间距、14px
 * 标题与列表间距）和 ListSkeleton 的列表外框。这样过渡屏撤掉、真实内容进来时
 * 只有填充被替换，边框、行高、左右留白都不动，用户看到的不是「换了一屏」，
 * 而是「同一屏填上了数据」。
 *
 * 骨架形态取自 URL hash 的路由提示：刷新时 hash 立即可读，比等后端会话更快，
 * 所以停在文件浏览页刷新不会先看到一份首页骨架再换成文件列表。
 * 这里不读任何业务数据：会话还没确认就把真实列表渲染出来，未登录时反而更跳。
 */
const props = defineProps({
  route: {
    type: Object,
    default: () => ({ view: "home" }),
  },
});

const SECTIONS = {
  peer: [{ key: "files", variant: "collapse", rows: 6, pills: 3 }],
  settings: [{ key: "settings", variant: "collapse", rows: 3, pills: 4 }],
  home: [
    { key: "servers", variant: "card", rows: 2, pills: 0 },
    { key: "discovered", variant: "card", rows: 3, pills: 1 },
  ],
};

const sections = computed(
  () => SECTIONS[props.route?.view] ?? SECTIONS.home,
);
</script>

<template>
  <div class="startup-skeleton" role="status" :aria-busy="true">
    <section v-for="(section, index) in sections" :key="section.key" class="startup-section">
      <el-skeleton animated :count="1">
        <template #template>
          <div class="startup-section__heading">
            <el-skeleton-item variant="rect" class="startup-heading__title" />
            <el-skeleton-item
              v-for="pill in section.pills"
              :key="pill"
              variant="rect"
              class="startup-heading__pill"
            />
            <!-- 首个区块的标题行右端保留一句可见提示：骨架块是灰条，光靠它
                 容易被读成「这一页就是空的」，一句话才说明是在读取。
                 放在标题行内而不是单独一行，撤骨架时不会把下面的列表顶动。 -->
            <span v-if="index === 0" class="startup-heading__label">{{ t("common.loading") }}</span>
          </div>
        </template>
      </el-skeleton>
      <list-skeleton :variant="section.variant" :rows="section.rows" />
    </section>
  </div>
</template>

<style scoped>
/* 与 HomeView 的 .home-view / .home-section 保持一致的节奏，
   列表外框与行尺寸交给 ListSkeleton，不在这里第二份定义。 */
.startup-skeleton {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 30px;
}

.startup-section {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.startup-section :deep(.el-skeleton) {
  display: flex;
  flex-direction: column;
}

.startup-section__heading {
  display: flex;
  align-items: center;
  gap: 12px;
}

/* 20px 等于真实区块标题那一行的实际高度：标题是 13px 粗体，旁边的数量角标
   钉在 20px，flex 居中后整行就是 20px。换成真实标题时列表不会被顶动。 */
.startup-heading__title {
  width: 160px;
  height: 20px;
  border-radius: 6px;
}

.startup-heading__pill {
  width: 64px;
  height: 20px;
  border-radius: 999px;
}

/* 靠到标题行右端，用静音色：它是状态说明，不该比即将出现的标题更抢眼。
   13px 低于行内 20px 的占位高度，所以换成真实内容时这一行不会变高。 */
.startup-heading__label {
  margin-left: auto;
  font-size: 13px;
  color: var(--app-text-muted);
}
</style>
