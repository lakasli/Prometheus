import { nextTick } from "vue";
import type { DOMWrapper } from "@vue/test-utils";

type SelectControl = Pick<DOMWrapper<Element>, "trigger" | "attributes">;

async function optionsFor(control: SelectControl) {
  if (control.attributes("aria-expanded") !== "true")
    await control.trigger("click");
  const list = document.getElementById(
    control.attributes("aria-controls") || "",
  );
  if (!list) throw new Error("Select did not open its option list");
  return Array.from(list.querySelectorAll<HTMLElement>('[role="option"]'));
}

export async function selectValue(
  control: SelectControl,
  value: string | number,
) {
  const option = (await optionsFor(control)).find(
    (item) => item.dataset.value === String(value),
  );
  if (!option) throw new Error(`Missing select option ${value}`);
  option.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  await nextTick();
}

export async function selectValues(control: SelectControl) {
  const values = (await optionsFor(control)).map((item) => item.dataset.value);
  await control.trigger("keydown", { key: "Escape" });
  return values;
}
