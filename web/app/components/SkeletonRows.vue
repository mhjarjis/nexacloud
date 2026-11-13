<script setup lang="ts">
withDefaults(defineProps<{ rows?: number; height?: number; label?: string }>(), {
  rows: 4,
  height: 18,
  label: 'Loading',
})
</script>

<template>
  <div class="skeleton-rows" role="status" :aria-label="label">
    <span
      v-for="i in rows"
      :key="i"
      class="skel"
      :style="{ height: `${height}px`, width: `${100 - ((i * 17) % 35)}%` }"
    />
  </div>
</template>

<style scoped>
.skeleton-rows { display: flex; flex-direction: column; gap: 14px; padding: 4px 0; }
</style>
