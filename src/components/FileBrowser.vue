<script setup>
import { ref } from 'vue';
import Browser from './PeerBrowser.vue';
const browser = ref(null);
defineExpose({ refresh: () => browser.value?.refresh() });

defineProps({
  peer: { type: Object, required: true },
  adapter: { type: Object, default: null },
  hideSidebar: { type: Boolean, default: false },
});
</script>

<template>
  <Browser ref="browser" :peer="peer" :adapter="adapter" :hide-sidebar="hideSidebar" />
</template>
