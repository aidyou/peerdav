import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compileStyle, parse } from "vue/compiler-sfc";

describe("settings dark appearance stylesheet", () => {
  it("inverts only the monochrome tray preview, never the whole document", () => {
    const source = readFileSync("src/components/SettingsView.vue", "utf8");
    const { descriptor } = parse(source);
    const compiled = descriptor.styles.map((style) => compileStyle({
      source: style.content,
      filename: "SettingsView.vue",
      id: "data-v-settings-test",
      scoped: style.scoped,
    }));
    for (const style of compiled) expect(style.errors).toEqual([]);
    const css = compiled.map((style) => style.code).join("\n");
    const inversionRules = [...css.matchAll(/([^{}]+)\{([^{}]*\bfilter\s*:\s*invert\(1\)[^{}]*)\}/g)];
    expect(inversionRules).toHaveLength(1);
    const selector = inversionRules[0][1].trim();
    expect(selector).toBe('html[data-theme="dark"] .tray-icon-preview--mono[data-v-settings-test]');
  });
});
