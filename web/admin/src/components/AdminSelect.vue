<script setup lang="ts" generic="T extends string | number">
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  useAttrs,
  useId,
  watch,
} from "vue";

defineOptions({ inheritAttrs: false });

const props = withDefaults(
  defineProps<{
    modelValue: T;
    options: { value: T; label: string; disabled?: boolean }[];
    disabled?: boolean;
    readonly?: boolean;
    placeholder?: string;
    name?: string;
  }>(),
  { disabled: false, readonly: false, placeholder: "请选择", name: undefined },
);
const emit = defineEmits<{
  "update:modelValue": [value: T];
  change: [value: T];
}>();
const attrs = useAttrs();
const uniqueId = useId();
const controlId = computed(() => String(attrs.id || `${uniqueId}-control`));
const listId = `${uniqueId}-options`;
const root = ref<HTMLElement>();
const trigger = ref<HTMLButtonElement>();
const list = ref<HTMLElement>();
const opened = ref(false);
const active = ref(-1);
const menuStyle = ref<Record<string, string>>({});
const selected = computed(() =>
  props.options.find((item) => item.value === props.modelValue),
);
const selectedLabel = computed(
  () => selected.value?.label || props.placeholder,
);
const fixed = computed(
  () =>
    props.readonly ||
    (props.options.length === 1 && !!selected.value) ||
    !props.options.some((item) => !item.disabled),
);
let search = "";
let searchTimer: ReturnType<typeof setTimeout> | undefined;
let resizeObserver: ResizeObserver | undefined;

function positionMenu() {
  if (!opened.value || !trigger.value) return;
  const box = trigger.value.getBoundingClientRect();
  const margin = 12;
  const gap = 6;
  if (
    box.height > 0 &&
    (box.bottom < margin || box.top > window.innerHeight - margin)
  ) {
    closeMenu();
    return;
  }
  const below = window.innerHeight - box.bottom - margin - gap;
  const above = box.top - margin - gap;
  const showAbove = below < 180 && above > below;
  const available = Math.max(40, showAbove ? above : below);
  const height = Math.min(300, available);
  const width = Math.min(
    Math.max(box.width, 160),
    window.innerWidth - margin * 2,
  );
  const left = Math.max(
    margin,
    Math.min(box.left, window.innerWidth - width - margin),
  );
  menuStyle.value = {
    width: `${width}px`,
    left: `${left}px`,
    maxHeight: `${height}px`,
    ...(showAbove
      ? { bottom: `${window.innerHeight - box.top + gap}px` }
      : { top: `${box.bottom + gap}px` }),
  };
}
function scrollActive() {
  const option = list.value?.querySelector<HTMLElement>(
    `[data-option-index="${active.value}"]`,
  );
  if (!option || !list.value) return;
  const menu = list.value;
  if (option.offsetTop < menu.scrollTop) menu.scrollTop = option.offsetTop;
  if (
    option.offsetTop + option.offsetHeight >
    menu.scrollTop + menu.clientHeight
  )
    menu.scrollTop = option.offsetTop + option.offsetHeight - menu.clientHeight;
}
async function openMenu() {
  if (props.disabled || fixed.value || opened.value) return;
  opened.value = true;
  const current = props.options.findIndex(
    (item) => item.value === props.modelValue && !item.disabled,
  );
  active.value =
    current >= 0 ? current : props.options.findIndex((item) => !item.disabled);
  positionMenu();
  document.addEventListener("pointerdown", outside, true);
  window.addEventListener("resize", positionMenu);
  window.addEventListener("scroll", positionMenu, true);
  if (typeof ResizeObserver !== "undefined" && trigger.value) {
    resizeObserver = new ResizeObserver(positionMenu);
    resizeObserver.observe(trigger.value);
  }
  await nextTick();
  scrollActive();
}
function closeMenu() {
  opened.value = false;
  search = "";
  clearTimeout(searchTimer);
  document.removeEventListener("pointerdown", outside, true);
  window.removeEventListener("resize", positionMenu);
  window.removeEventListener("scroll", positionMenu, true);
  resizeObserver?.disconnect();
  resizeObserver = undefined;
}
function outside(event: PointerEvent) {
  const target = event.target as Node | null;
  if (target && !root.value?.contains(target) && !list.value?.contains(target))
    closeMenu();
}
function choose(index: number) {
  const option = props.options[index];
  if (!option || option.disabled || props.disabled) return;
  if (option.value !== props.modelValue) {
    emit("update:modelValue", option.value);
    emit("change", option.value);
  }
  closeMenu();
  trigger.value?.focus({ preventScroll: true });
}
function move(direction: 1 | -1) {
  const count = props.options.length;
  for (let step = 1; step <= count; step++) {
    const index = (active.value + direction * step + count) % count;
    if (!props.options[index]?.disabled) {
      active.value = index;
      void nextTick(scrollActive);
      return;
    }
  }
}
async function onKeydown(event: KeyboardEvent) {
  if (props.disabled || fixed.value || event.isComposing) return;
  if (event.key === "Tab") {
    closeMenu();
    return;
  }
  if (event.key === "Escape") {
    if (opened.value) event.preventDefault();
    closeMenu();
    return;
  }
  if (
    ["ArrowDown", "ArrowUp", "Home", "End", "Enter", " "].includes(event.key)
  ) {
    event.preventDefault();
    if (!opened.value) {
      await openMenu();
      if (event.key !== "Home" && event.key !== "End") return;
    } else if (event.key === "Enter" || event.key === " ") {
      choose(active.value);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp")
      move(event.key === "ArrowDown" ? 1 : -1);
    if (event.key === "Home")
      active.value = props.options.findIndex((item) => !item.disabled);
    if (event.key === "End") {
      for (let i = props.options.length - 1; i >= 0; i--) {
        if (!props.options[i]?.disabled) {
          active.value = i;
          break;
        }
      }
    }
    await nextTick();
    scrollActive();
    return;
  }
  if (
    event.key.length === 1 &&
    !event.ctrlKey &&
    !event.metaKey &&
    !event.altKey
  ) {
    event.preventDefault();
    await openMenu();
    clearTimeout(searchTimer);
    search += event.key.toLocaleLowerCase();
    searchTimer = setTimeout(() => {
      search = "";
    }, 700);
    const index = props.options.findIndex(
      (item) =>
        !item.disabled && item.label.toLocaleLowerCase().startsWith(search),
    );
    if (index >= 0) active.value = index;
    await nextTick();
    scrollActive();
  }
}

watch(
  () => props.disabled || fixed.value,
  (value) => {
    if (value) closeMenu();
  },
);
watch(
  () => [props.modelValue, props.options] as const,
  ([value, options], previous) => {
    if (!opened.value) return;
    const focusedValue =
      previous?.[0] === value ? previous[1][active.value]?.value : value;
    const index = options.findIndex(
      (item) => item.value === focusedValue && !item.disabled,
    );
    active.value =
      index >= 0 ? index : props.options.findIndex((item) => !item.disabled);
    void nextTick(() => {
      positionMenu();
      scrollActive();
    });
  },
  { deep: true },
);
onBeforeUnmount(closeMenu);
</script>

<template>
  <span
    ref="root"
    class="admin-select"
    :class="{ 'is-open': opened, 'is-fixed': fixed }"
  >
    <input
      v-if="fixed"
      v-bind="attrs"
      :id="controlId"
      class="admin-select-control"
      :value="selectedLabel"
      readonly
      :disabled="disabled"
    />
    <button
      v-else
      v-bind="attrs"
      :id="controlId"
      ref="trigger"
      type="button"
      role="combobox"
      class="admin-select-control"
      :disabled="disabled"
      aria-haspopup="listbox"
      :aria-expanded="opened"
      :aria-controls="opened ? listId : undefined"
      :aria-activedescendant="
        opened && active >= 0 ? `${listId}-${active}` : undefined
      "
      @click="opened ? closeMenu() : openMenu()"
      @keydown="onKeydown"
      @blur="closeMenu"
    >
      <span class="admin-select-label">{{ selectedLabel }}</span>
      <svg
        class="admin-select-chevron"
        viewBox="0 0 20 20"
        fill="none"
        aria-hidden="true"
      >
        <path
          d="m6 8 4 4 4-4"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
    <input
      v-if="name"
      type="hidden"
      :name="name"
      :value="modelValue"
      :disabled="disabled"
    />
    <Teleport to="body">
      <ul
        v-if="opened"
        :id="listId"
        ref="list"
        class="admin-select-menu"
        role="listbox"
        :aria-label="
          attrs['aria-label'] ? `${attrs['aria-label']}选项` : '可选项'
        "
        :style="menuStyle"
        @pointerdown.prevent
      >
        <li
          v-for="(option, index) in options"
          :id="`${listId}-${index}`"
          :key="`${typeof option.value}:${option.value}`"
          :data-option-index="index"
          :data-value="option.value"
          role="option"
          :aria-selected="option.value === modelValue"
          :aria-disabled="option.disabled || undefined"
          :class="{
            'is-active': active === index,
            'is-selected': option.value === modelValue,
            'is-disabled': option.disabled,
          }"
          @pointermove="!option.disabled && (active = index)"
          @click.stop="choose(index)"
        >
          <span>{{ option.label }}</span
          ><svg
            v-if="option.value === modelValue"
            viewBox="0 0 20 20"
            fill="none"
            aria-hidden="true"
          >
            <path
              d="m5 10 3 3 7-7"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </li>
      </ul>
    </Teleport>
  </span>
</template>

<style scoped>
.admin-select {
  display: block;
  min-width: 0;
  width: 100%;
}
.admin-select-control {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  width: 100%;
  min-height: 44px;
  padding: 10px 13px;
  border: 1px solid var(--line, #dce2ef);
  border-radius: 10px;
  background: #fff;
  color: var(--ink, #26344b);
  font-size: 14px;
  font-weight: 400;
  line-height: 1.5;
  text-align: left;
  box-shadow: 0 1px 2px #25355b04;
  transition:
    border-color 0.15s,
    box-shadow 0.15s,
    background 0.15s;
}
button.admin-select-control {
  cursor: pointer;
}
button.admin-select-control:hover:not(:disabled) {
  border-color: #9ba9cc;
  background: #fbfcff;
}
.admin-select-control:focus-visible,
.is-open .admin-select-control {
  outline: none;
  border-color: var(--primary, #53699c);
  box-shadow: 0 0 0 3px #53699c1a;
}
.admin-select-control:disabled {
  opacity: 1;
  color: #738098;
  background: #f3f5fa;
  border-color: #e3e7f0;
  cursor: not-allowed;
}
.is-fixed .admin-select-control {
  background: #f7f8fc;
  color: #53617b;
  box-shadow: none;
}
.admin-select-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.admin-select-chevron {
  flex: 0 0 18px;
  width: 18px;
  height: 18px;
  color: #7384a6;
  transition: transform 0.15s;
}
.is-open .admin-select-chevron {
  transform: rotate(180deg);
}
.admin-select-menu {
  position: fixed;
  z-index: 1100;
  display: block;
  box-sizing: border-box;
  padding: 6px;
  margin: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  border: 1px solid #dce2ef;
  border-radius: 12px;
  background: #fff;
  color: #26344b;
  box-shadow:
    0 12px 32px #24365324,
    0 3px 8px #2436530d;
  list-style: none;
  scrollbar-width: thin;
  scrollbar-color: #c7d0e3 transparent;
}
.admin-select-menu li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  min-height: 38px;
  padding: 9px 11px;
  border-radius: 7px;
  color: #3a4965;
  font-size: 14px;
  font-weight: 400;
  line-height: 1.5;
  overflow-wrap: anywhere;
  cursor: pointer;
}
.admin-select-menu li.is-selected {
  color: #354f86;
  background: #f0f3fb;
  font-weight: 500;
}
.admin-select-menu li.is-active {
  color: #2c4478;
  background: #e9eef9;
}
.admin-select-menu li.is-disabled {
  color: #8993a7;
  background: transparent;
  cursor: not-allowed;
}
.admin-select-menu svg {
  width: 18px;
  height: 18px;
  flex: 0 0 18px;
  color: #53699c;
}
@media (prefers-reduced-motion: reduce) {
  .admin-select-control,
  .admin-select-chevron {
    transition: none;
  }
}
</style>
