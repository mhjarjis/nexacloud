<script setup lang="ts">
// Calm blue note, e.g. "Creating and editing sites arrives in v0.2".
</script>

<template>
  <p class="note">
    <AppIcon name="info" :size="16" :stroke="2" />
    <span><slot /></span>
  </p>
</template>
