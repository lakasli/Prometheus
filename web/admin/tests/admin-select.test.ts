// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, type VueWrapper } from "@vue/test-utils";
import { nextTick } from "vue";
import AdminSelect from "../src/components/AdminSelect.vue";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  document.body.replaceChildren();
  vi.restoreAllMocks();
});
const numericOptions = [
  { value: 1, label: "一个任务" },
  { value: 2, label: "两个任务", disabled: true },
  { value: 3, label: "三个任务" },
];

describe("admin select", () => {
  it("commits numeric keyboard choices, skips disabled values and follows external model updates", async () => {
    wrapper = mount(AdminSelect, {
      attachTo: document.body,
      props: { modelValue: 1, options: numericOptions },
      attrs: { "aria-label": "任务数量", "aria-describedby": "quantity-help" },
    });
    const control = wrapper.get('[role="combobox"]');
    expect(control.attributes("aria-describedby")).toBe("quantity-help");
    await control.trigger("keydown", { key: "ArrowDown" });
    expect(control.attributes("aria-expanded")).toBe("true");
    expect(
      document.getElementById(control.attributes("aria-controls")!),
    ).not.toBeNull();
    await control.trigger("keydown", { key: "ArrowDown" });
    const activeId = control.attributes("aria-activedescendant");
    expect(document.getElementById(activeId!)?.textContent).toContain(
      "三个任务",
    );
    await control.trigger("keydown", { key: "Enter" });
    expect(wrapper.emitted("update:modelValue")).toEqual([[3]]);
    expect(wrapper.emitted("change")).toEqual([[3]]);
    expect(control.attributes("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(control.element);
    await wrapper.setProps({ modelValue: 3 });
    expect(control.text()).toBe("三个任务");
    await wrapper.setProps({ modelValue: 1 });
    expect(control.text()).toBe("一个任务");
  });

  it("dismisses Escape, Tab and outside clicks without committing the highlighted choice", async () => {
    wrapper = mount(AdminSelect, {
      attachTo: document.body,
      props: { modelValue: 1, options: numericOptions },
    });
    const control = wrapper.get('[role="combobox"]');
    await control.trigger("click");
    await control.trigger("keydown", { key: "End" });
    await control.trigger("keydown", { key: "Escape" });
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    expect(document.querySelector('[role="listbox"]')).toBeNull();
    await control.trigger("click");
    await control.trigger("keydown", { key: "Tab" });
    expect(control.attributes("aria-expanded")).toBe("false");
    await control.trigger("click");
    document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    await nextTick();
    expect(control.attributes("aria-expanded")).toBe("false");
  });

  it("renders a single fixed choice without a dropdown and never opens a disabled control", async () => {
    wrapper = mount(AdminSelect, {
      props: {
        modelValue: "inherit",
        options: [{ value: "inherit", label: "继承当前模型" }],
      },
    });
    const fixed = wrapper.get("input[readonly]");
    expect((fixed.element as HTMLInputElement).value).toBe("继承当前模型");
    expect(wrapper.find('[role="combobox"]').exists()).toBe(false);
    await wrapper.setProps({
      options: [
        { value: "inherit", label: "继承当前模型" },
        { value: "other", label: "其他模型" },
      ],
      disabled: true,
    });
    const control = wrapper.get('[role="combobox"]');
    expect(control.attributes("disabled")).toBeDefined();
    await control.trigger("click");
    await control.trigger("keydown", { key: "ArrowDown" });
    expect(document.querySelector('[role="listbox"]')).toBeNull();
  });

  it("uses a viewport-level menu above a low trigger and keeps the form value separate from its label", async () => {
    wrapper = mount(AdminSelect, {
      attachTo: document.body,
      props: { modelValue: 1, options: numericOptions, name: "quantity" },
    });
    const control = wrapper.get('[role="combobox"]');
    vi.spyOn(control.element, "getBoundingClientRect").mockReturnValue({
      x: 900,
      y: 740,
      top: 740,
      left: 900,
      right: 1180,
      bottom: 784,
      width: 280,
      height: 44,
      toJSON: () => ({}),
    });
    await control.trigger("click");
    const menu = document.querySelector<HTMLElement>('[role="listbox"]')!;
    expect(menu.parentElement).toBe(document.body);
    expect(menu.style.bottom).not.toBe("");
    expect(menu.style.left).toBe(
      `${Math.max(12, window.innerWidth - 280 - 12)}px`,
    );
    expect(
      (wrapper.get('input[name="quantity"]').element as HTMLInputElement).value,
    ).toBe("1");
    await wrapper.setProps({ disabled: true });
    expect(document.querySelector('[role="listbox"]')).toBeNull();
  });
});
