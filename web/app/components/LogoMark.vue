<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), { size: 28 })
</script>

<template>
  <svg :width="size" :height="size" viewBox="0 0 28 28" aria-hidden="true" focusable="false">
    <rect x="1" y="1" width="26" height="26" rx="7" fill="#3DDC97" />
    <path d="M8.5 19V9l11 10V9" fill="none" stroke="#06130D" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
</template>
