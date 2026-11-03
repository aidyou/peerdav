import { createApp } from "vue";
import App from "./App.vue";
// Element Plus dark palette: applied when <html> carries the `dark` class,
// which the theme module toggles based on the resolved theme.
import "element-plus/theme-chalk/dark/css-vars.css";
import "element-plus/es/components/message/style/css";
// Detached MessageBox styles for confirmations that render their own heading.
import "./assets/confirm.css";

createApp(App).mount("#app");
