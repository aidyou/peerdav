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
      <label class="settings-row"><span class="settings-row__label">{{ t('cloud.proxy') }}</span>
        <el-select v-model="mode" :disabled="busy" :aria-label="t('cloud.proxy')" @change="saved = false">
          <el-option value="direct" :label="t('cloud.proxyDirect')" />
          <el-option v-if="connectionId" value="client" :label="t('network.client')" />
          <template v-else><el-option value="environment" :label="t('network.system')" /><el-option value="custom" label="HTTP" /></template>
        </el-select>
      </label>
      <template v-if="!connectionId && mode === 'custom'">
        <label class="settings-row"><span class="settings-row__label">{{ t('cloud.proxyUrl') }}</span><el-input v-model="url" :disabled="busy" placeholder="http://127.0.0.1:7890" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('network.username') }}</span><el-input v-model="username" :disabled="busy" autocomplete="off" /></label>
        <label class="settings-row"><span class="settings-row__label">{{ t('network.password') }}</span><el-input v-model="password" :disabled="busy" type="password" autocomplete="new-password" :placeholder="t('network.passwordHint')" /></label>
      </template>
    </div>
    <p class="settings-group__footnote">{{ t(connectionId ? 'network.driveHint' : 'network.systemHint') }}</p>
    <el-button native-type="submit" :loading="busy" :disabled="busy || !loaded || (mode === 'custom' && !url.trim())">{{ t('network.save') }}</el-button>
    <span v-if="saved" role="status">{{ t('network.saved') }}</span>
  </form>
</template>
