<script setup lang="ts">
defineProps<{ name: string; size?: number }>();
const paths: Record<string, string> = {
  grid: "M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z",
  settings:
    "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M12 2v3 M12 19v3 M2 12h3 M19 12h3 M5 5l2 2 M17 17l2 2 M5 19l2-2 M17 7l2-2",
  chat: "M21 11.5a8.5 8.5 0 0 1-8.5 8.5H4l-2 2V11.5a9.5 9.5 0 0 1 19 0z M7 11h10 M7 15h6",
  channel: "M5 3h14v18H5z M9 7h6 M9 11h6 M11 17h2",
  clock: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18 M12 7v5l3 2",
  arrow: "M5 12h14 M13 6l6 6-6 6",
  check: "M5 12l4 4L19 6",
  close: "M6 6l12 12 M6 18L18 6",
  refresh:
    "M20 7v5h-5 M4 17v-5h5 M6.1 7a7 7 0 0 1 11.55-2L20 8 M4 16l2.35 3A7 7 0 0 0 17.9 17",
  bolt: "M13 2L4 14h7l-1 8 10-12h-7z",
  plus: "M12 5v14 M5 12h14",
  shield: "M12 2l8 4v6c0 5-8 10-8 10S4 17 4 12V6z M8 12l3 3 5-6",
  chevron: "M9 5l7 7-7 7",
  logout: "M9 4H4v16h5 M14 8l4 4-4 4 M8 12h11",
  cpu: "M6 6h12v12H6z M9 9h6v6H9z M9 2v4 M15 2v4 M9 18v4 M15 18v4 M2 9h4 M2 15h4 M18 9h4 M18 15h4",
  key: "M8 3a5 5 0 1 0 3.8 8.2L20 19l2-2-2-2 1-1-3-3-2 2-2.8-2.8A5 5 0 0 0 8 3z M7 7h.01",
  circle: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18",
  menu: "M4 6h16 M4 12h16 M4 18h16",
  save: "M4 3h13l4 4v14H3V3z M7 3v6h10V3 M7 21v-8h10v8",
};
</script>

<template>
  <svg
    :width="size || 20"
    :height="size || 20"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path :d="paths[name] || paths.circle" />
  </svg>
</template>
