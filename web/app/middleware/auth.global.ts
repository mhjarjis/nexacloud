// Every page except /login needs a valid session. The result is cached in
// useAuthState() so navigation between pages doesn't re-check each time;
// useApi() resets it to false on any 401.
export default defineNuxtRouteMiddleware(async (to) => {
  if (to.path === '/login') return

  const authed = useAuthState()
  if (authed.value === true) return

  try {
    await $fetch('/api/auth/me', { credentials: 'include' })
    authed.value = true
  } catch {
    authed.value = false
    return navigateTo({ path: '/login', query: to.fullPath !== '/' ? { next: to.fullPath } : undefined })
  }
})
