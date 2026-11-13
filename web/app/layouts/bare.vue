<template>
  <main class="bare">
    <slot />
  </main>
</template>

<style scoped>
.bare {
  min-height: 100vh;
  display: grid;
  place-items: center;
  padding: 24px 16px;
  background:
    radial-gradient(700px 400px at 50% -10%, rgba(61, 220, 151, .07), transparent 70%),
    var(--bg);
}
</style>
