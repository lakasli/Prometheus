import type { Locator, Page } from "@playwright/test";

export async function selectValue(
  page: Page,
  control: Locator,
  value: string | number,
) {
  if ((await control.getAttribute("aria-expanded")) !== "true")
    await control.click();
  const listId = await control.getAttribute("aria-controls");
  if (!listId) throw new Error("Select did not open its option list");
  await page
    .locator(`[id=${JSON.stringify(listId)}]`)
    .locator(`[role="option"][data-value=${JSON.stringify(String(value))}]`)
    .click();
}
