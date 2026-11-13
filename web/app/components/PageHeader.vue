<script setup lang="ts">
defineProps<{ title: string; crumb?: string }>()
</script>

<template>
  <header class="page-head">
    <div class="titles">
      <p v-if="crumb" class="lbl">{{ crumb }}</p>
      <h1>{{ title }}</h1>
    </div>
    <div v-if="$slots.default" class="actions">
      <slot />
    </div>
  </header>
</template>
