<script setup>
import { onMounted, ref } from 'vue';
import { invoke } from '../lib/invoke';
import { useAppContext } from '../lib/appContext';
import { t } from '../lib/i18n';
const props = defineProps({ connectionId: { type: String, default: '' } });
const app = useAppContext();
const busy = ref(true);
const mode = ref(props.connectionId ? 'client' : 'environment');
const url = ref('');
const username = ref('');
const password = ref('');
const saved = ref(false);
const loaded = ref(false);
onMounted(async () => {
  try {
    if (props.connectionId) {
      mode.value = await invoke('drive_proxy_settings', { connectionId: props.connectionId }) ? 'direct' : 'client';
    } else {
      const proxy = await invoke('client_proxy_settings');
      mode.value = proxy.mode; url.value = proxy.url || ''; username.value = proxy.username || '';
    }
    loaded.value = true;
  } catch (error) { app.handleFailure(error); }
  finally { busy.value = false; }
});
async function save() {
  if (!loaded.value || busy.value) return;
  busy.value = true; saved.value = false;
  try {
    if (props.connectionId) {
      await invoke('save_drive_proxy_settings', { connectionId: props.connectionId, direct: mode.value === 'direct' });
    } else {
      const proxy = mode.value === 'custom' ? { mode: 'custom', url: url.value.trim(), username: username.value || null, password: password.value || null } : { mode: mode.value };
      await invoke('save_client_proxy_settings', { proxy });
    }
    password.value = ''; saved.value = true;
  } catch (error) { app.handleFailure(error); }
  finally { busy.value = false; }
}
</script>

<template>
  <form class="settings-group network-proxy-settings" @submit.prevent="save">
    <h3 class="group-title">{{ t('network.title') }}</h3>
    <div class="settings-rows">
      <label class="settings-row">
        <span class="settings-row__label">{{ t('cloud.proxy') }}</span>
        <el-select class="row-control" v-model="mode" :disabled="busy" :aria-label="t('cloud.proxy')" @change="saved = false">
          <el-option value="direct" :label="t('cloud.proxyDirect')" />
          <el-option v-if="connectionId" value="client" :label="t('network.client')" />
          <template v-else><el-option value="environment" :label="t('network.system')" /><el-option value="custom" label="HTTP" /></template>
        </el-select>
      </label>
      <template v-if="!connectionId && mode === 'custom'">
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.proxyUrl') }}</span><el-input class="row-control" v-model="url" :disabled="busy" placeholder="http://127.0.0.1:7890" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('network.username') }}</span><el-input class="row-control" v-model="username" :disabled="busy" autocomplete="off" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('network.password') }}</span><el-input class="row-control" v-model="password" :disabled="busy" type="password" autocomplete="new-password" :placeholder="t('network.passwordHint')" /></label>
      </template>
      <!-- 保存与「已保存」提示收进列表尾行并右对齐，与通用面板其它表单的提交行一致；
           控件统一用 row-control 对齐到 240px 取值列，不再让按钮浮在脚注之外。 -->
      <div class="settings-row settings-row--actions">
        <span v-if="saved" class="settings-row__value" role="status">{{ t('network.saved') }}</span>
        <el-button type="primary" native-type="submit" :loading="busy" :disabled="busy || !loaded || (mode === 'custom' && !url.trim())">{{ t('network.save') }}</el-button>
      </div>
    </div>
    <p class="settings-group__footnote">{{ t(connectionId ? 'network.driveHint' : 'network.systemHint') }}</p>
  </form>
</template>

<style scoped>
/* 行式列表原语在 SettingsView 里是 scoped，子组件拿不到，故在此自带一份
   （与 CloudSettings / CloudSyncDrives 同模式），让这块与通用面板相邻分组一致。 */
.settings-rows {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--app-border);
  border-radius: 10px;
  background: var(--app-surface-3);
  overflow: hidden;
}

/* 52px 行高容纳右侧控件并留出点击余量，与其它设置行同列同高。 */
.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  min-width: 0;
  min-height: 52px;
  padding: 10px 14px;
}

.settings-row + .settings-row {
  border-top: 1px solid var(--app-border);
}

.settings-row__label {
  font-size: 13px;
  font-weight: 600;
  color: var(--app-text-strong);
}

.settings-row__value {
  font-size: 13px;
  color: var(--app-text-secondary);
}

/* 动作尾行不占标题列，控件右对齐，读起来是这张表单的提交行。 */
.settings-row--actions {
  justify-content: flex-end;
}

/* 右侧控件列统一 240px，与通用面板其它行的右边界对齐。 */
.settings-row .row-control {
  width: 240px;
  max-width: 100%;
}

/* 注释性说明统一用斜体与小一号字，跟字段标题、取值区分开。 */
.settings-group__footnote {
  margin: 8px 2px 0;
  font-size: 12px;
  font-style: italic;
  line-height: 1.5;
  color: var(--app-text-secondary);
}
</style>
