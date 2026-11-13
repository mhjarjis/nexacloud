<script setup lang="ts">
// "Jobs" became "Activity" in v0.2; keep old links working.
definePageMeta({
  middleware: () => navigateTo('/activity', { replace: true }),
})
</script>

<template>
  <div />
</template>
