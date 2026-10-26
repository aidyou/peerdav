import { describe, expect, it } from "vitest";
import { h } from "vue";
import { mount } from "@vue/test-utils";
import { ElCollapse, ElCollapseItem } from "element-plus";

// SettingsView lays the share rows out as three columns inside the collapse
// header. Element Plus wraps the `#title` slot in its own span and appends the
// toggle arrow as a sibling, so the column grid has to target
// `.el-collapse-item__title` rather than the header. These tests pin that DOM
// contract: if Element Plus changes it, the CSS must be revisited.
function mountCollapse(active = false) {
  const Probe = {
    render() {
      return h(
        ElCollapse,
        { modelValue: active ? ["share-1"] : [] },
        () => [
          h(
            ElCollapseItem,
            { name: "share-1" },
            {
              title: () => [
                h("div", { class: "share-item__main" }, "main"),
                h("div", { class: "share-item__perms" }, "perms"),
                h("div", { class: "share-item__actions" }, "actions"),
              ],
              default: () => h("ul", { class: "share-item__rules" }, "rules"),
            },
          ),
        ],
      );
    },
  };
  return mount(Probe);
}

describe("share collapse header contract", () => {
  it("wraps the title slot in one container and keeps the arrow last", () => {
    const wrapper = mountCollapse();
    const header = wrapper.find(".el-collapse-item__header");
    const children = header.findAll(":scope > *").map((el) => el.classes());

    expect(children).toHaveLength(2);
    expect(children[0]).toContain("el-collapse-item__title");
    expect(children[1]).toContain("el-collapse-item__arrow");
    wrapper.unmount();
  });

  it("keeps the three share columns inside the title container", () => {
    const wrapper = mountCollapse();
    const columns = wrapper
      .find(".el-collapse-item__title")
      .findAll(":scope > *")
      .map((el) => el.classes().join(" "));

    expect(columns).toEqual([
      "share-item__main",
      "share-item__perms",
      "share-item__actions",
    ]);
    wrapper.unmount();
  });

  it("puts the arrow on the right and marks the open row with is-active", () => {
    // The row separators and the header grid depend on the arrow living at the
    // end of the header, which Element Plus enables by default for collapse.
    const closed = mountCollapse();
    expect(closed.find(".el-collapse").classes()).toContain(
      "el-collapse-icon-position-right",
    );
    expect(closed.find(".el-collapse-item").classes()).not.toContain(
      "is-active",
    );
    closed.unmount();

    const opened = mountCollapse(true);
    expect(opened.find(".el-collapse-item").classes()).toContain("is-active");
    opened.unmount();
  });
});
