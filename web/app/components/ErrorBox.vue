<script setup lang="ts">
defineProps<{ message: string; retrying?: boolean }>()
const emit = defineEmits<{ retry: [] }>()
</script>

<template>
  <div class="error-box" role="alert">
    <AppIcon name="alert" :size="18" :stroke="2" />
    <span class="grow">{{ message }}</span>
    <button type="button" class="btn btn-s" :disabled="retrying" @click="emit('retry')">
      <span v-if="retrying" class="spinner" aria-hidden="true" />
      Try again
    </button>
  </div>
</template>
