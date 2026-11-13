// Drive the real panel in headless Chrome against the e2e container and
// screenshot every page.
//   KEEP=1 bash tests/docker/run-e2e.sh     # prints the password at the end
//   node tests/docker/ui-check.mjs <password> [outDir]
import { spawn } from 'node:child_process'
import { mkdirSync, writeFileSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const PASSWORD = process.argv[2]
const OUT = process.argv[3] || 'tests/docker/screenshots'
const BASE = process.env.BASE || 'https://127.0.0.1:18443'
const CHROME = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const PORT = 9444
if (!PASSWORD) { console.error('usage: node ui-check.mjs <password> [outDir]'); process.exit(2) }
mkdirSync(OUT, { recursive: true })

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const profile = mkdtempSync(join(tmpdir(), 'nc-chrome-'))
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${PORT}`, `--user-data-dir=${profile}`,
  '--ignore-certificate-errors', '--no-first-run', '--window-size=1440,1000', 'about:blank',
], { stdio: 'ignore' })

let failures = 0
const ok = (msg) => console.log(`  \x1b[32m✓\x1b[0m ${msg}`)
const bad = (msg) => { failures++; console.log(`  \x1b[31m✗ ${msg}\x1b[0m`) }
const expect = (cond, msg) => (cond ? ok(msg) : bad(msg))

try {
  let list
  for (let i = 0; i < 50; i++) {
    try { list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json(); if (list.find((t) => t.type === 'page')) break } catch {}
    await sleep(200)
  }
  const ws = new WebSocket(list.find((t) => t.type === 'page').webSocketDebuggerUrl)
  await new Promise((r) => ws.addEventListener('open', r))
  let id = 0
  const pending = new Map()
  const consoleErrors = []
  ws.addEventListener('message', (m) => {
    const d = JSON.parse(m.data)
    if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id) }
    if (d.method === 'Runtime.exceptionThrown') consoleErrors.push(d.params.exceptionDetails.text)
    if (d.method === 'Runtime.consoleAPICalled' && d.params.type === 'error') consoleErrors.push(d.params.args.map((a) => a.value ?? a.description).join(' '))
  })
  const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })) })
  const ev = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: `(async () => { ${expr} })()`, awaitPromise: true, returnByValue: true })
    return r.result?.result?.value ?? r.result?.exceptionDetails?.exception?.description
  }
  const text = () => ev('return document.body.innerText')
  const shot = async (name) => {
    const r = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true })
    writeFileSync(join(OUT, `${name}.png`), Buffer.from(r.result.data, 'base64'))
  }
  const go = async (path, wait = 2500) => { await send('Page.navigate', { url: BASE + path }); await sleep(wait) }
  await send('Runtime.enable'); await send('Page.enable')

  console.log('\n▶ Login')
  await go('/')
  expect((await ev('return location.pathname')) === '/login', 'unauthenticated visit redirects to /login')
  await shot('01-login')
  await ev(`const i=document.querySelector('input[type=password]'); i.value='definitely-wrong'; i.dispatchEvent(new Event('input')); document.querySelector('form').requestSubmit(); return 1`)
  await sleep(1500)
  expect(/wrong password/i.test(await text()), 'wrong password shows an error')
  await ev(`const i=document.querySelector('input[type=password]'); i.value=${JSON.stringify(PASSWORD)}; i.dispatchEvent(new Event('input')); document.querySelector('form').requestSubmit(); return 1`)
  await sleep(3000)
  expect((await ev('return location.pathname')) === '/', 'correct password lands on the dashboard')

  console.log('\n▶ Pages')
  const pages = [
    ['/', '02-dashboard', ['shop.test.local', 'Firewall']],
    ['/sites', '03-sites', ['shop.test.local', 'portal.test.local', 'ws.test.local', 'Laravel']],
    ['/php', '04-php', ['8.3']],
    ['/services', '05-services', ['Nginx', 'MySQL', 'PostgreSQL']],
    ['/databases', '06-databases', ['shop_prod', 'portal_db']],
    ['/security', '07-security', ['UFW']],
    ['/jobs', '08-jobs', []],
  ]
  for (const [path, name, needles] of pages) {
    await go(path, 3000)
    const t = await text()
    const missing = needles.filter((n) => !t.includes(n))
    const errBox = await ev(`return !!document.querySelector('.error-box, [role=alert].error')`)
    expect(missing.length === 0 && !errBox, `${path} renders${missing.length ? ' — missing: ' + missing.join(', ') : ''}${errBox ? ' — shows an error box' : ''}`)
    await shot(name)
  }

  console.log('\n▶ Back up a database from the UI')
  await go('/databases', 3000)
  const clicked = await ev(`const r=[...document.querySelectorAll('tbody tr')].find(t=>t.querySelector('td')?.textContent.trim()==='portal_db'); const b=r?.querySelector('button'); if(!b) return false; b.click(); return true`)
  expect(clicked === true, 'clicked "Back up now" for portal_db')
  let final = ''
  for (let i = 0; i < 20; i++) { await sleep(1000); final = await text(); if (/done|failed/i.test(final)) break }
  expect(/done/i.test(final) && !/failed/i.test(final), 'backup job finished in the UI')
  await shot('09-databases-after-backup')

  console.log('\n▶ Restart a service from the UI')
  await go('/services', 3000)
  await ev(`document.querySelector('[aria-label="Restart Nginx"]').click(); return 1`)
  await sleep(400)
  expect(!!(await ev(`return document.querySelector('.confirm-ask')?.innerText`)), 'inline confirm appears (no browser dialog)')
  await ev(`document.querySelector('.confirm-ask .btn-danger').click(); return 1`)
  await sleep(3000)
  expect(/restarted/i.test(await text()), 'nginx restarted message')

  console.log('\n▶ Phone width')
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true })
  await go('/', 3000)
  const overflow = await ev('return document.documentElement.scrollWidth > window.innerWidth + 1')
  expect(!overflow, 'dashboard has no horizontal scroll at 390 px')
  await shot('10-dashboard-mobile')
  await send('Emulation.clearDeviceMetricsOverride')

  console.log('\n▶ Sign out')
  await go('/', 2500)
  await ev(`const b=[...document.querySelectorAll('button')].find(b=>b.textContent.includes('Sign out')); b?.click(); return 1`)
  await sleep(2000)
  expect((await ev('return location.pathname')) === '/login', 'sign out returns to /login')

  const realErrors = consoleErrors.filter((e) => !/401|Unauthorized|wrong password|Failed to load resource/i.test(e))
  expect(realErrors.length === 0, `no unexpected console errors${realErrors.length ? ': ' + realErrors.slice(0, 3).join(' | ') : ''}`)
  ws.close()
} catch (e) {
  bad(`driver crashed: ${e?.stack || e}`)
} finally {
  chrome.kill()
}
console.log(failures ? `\n\x1b[31m${failures} UI check(s) failed\x1b[0m` : `\n\x1b[32mAll UI checks passed.\x1b[0m Screenshots in ${OUT}/`)
process.exit(failures ? 1 : 0)
