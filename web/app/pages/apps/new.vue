<script setup lang="ts">
// The new-project form lives on the Projects page (design board 02).
await navigateTo('/projects?new=1', { replace: true })
</script>

<template>
  <p class="small muted">Moved to <NuxtLink to="/projects?new=1">Projects → New project</NuxtLink>.</p>
</template>
