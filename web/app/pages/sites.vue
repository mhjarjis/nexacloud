<script setup lang="ts">
// Sites now live under Domains & SSL (domains) and Projects (everything else).
const route = useRoute()
await navigateTo({ path: '/domains', query: route.query }, { replace: true })
</script>

<template>
  <p class="small muted">Moved to <NuxtLink to="/domains">Domains &amp; SSL</NuxtLink>.</p>
</template>
