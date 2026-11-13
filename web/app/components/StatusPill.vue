<script setup lang="ts">
import type { Tone } from '~/types/api'

defineProps<{ tone: Tone; label: string }>()
</script>

<template>
  <span class="pill" :class="tone">{{ label }}</span>
</template>
