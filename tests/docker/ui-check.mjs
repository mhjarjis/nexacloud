// Drive the real panel in headless Chrome against the e2e container and
// screenshot every page.
//   KEEP=1 bash tests/docker/run-e2e.sh     # prints the password at the end
//   node tests/docker/ui-check.mjs <password> [outDir]
//   BASE=https://127.0.0.1:18445 (default) — the e2e container's panel
import { spawn } from 'node:child_process'
import { mkdirSync, writeFileSync, mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const PASSWORD = process.argv[2]
const OUT = process.argv[3] || 'tests/docker/screenshots'
const BASE = process.env.BASE || 'https://127.0.0.1:18445'
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
    ['/security', '07-security', ['UFW', 'Hide nginx version']],
    ['/activity', '08-activity', ['Timeline', 'Hide nginx version', 'Roll back: Hide nginx version']],
    ['/advisor', '11-advisor', ['Advisor', 'Tuned for']],
    ['/backups', '12-backups', ['Backup plans', 'Nightly', 'Destinations', 'Synology (test)', 'Restore points']],
    ['/projects', '13-deployments', ['Deployments', 'app.test.local', '/srv/git/app.git']],
    ['/projects/app.test.local', '14-project', ['Deploy now', 'Visit site', 'HTTP check', 'Response time · 24 h', 'PHP project settings', 'Latest errors']],
    ['/projects/app.test.local?tab=deploys', '14b-project-deploys', ['Recent runs', 'Releases on disk', 'Switch release']],
    ['/projects/app.test.local?tab=env', '14c-project-env', ['.env', 'Add variable']],
    ['/projects/app.test.local?tab=logs', '14d-project-logs', ['Live tail', 'Download']],
    ['/monitoring', '17-monitoring', ['Monitoring', 'shop.test.local', 'Where to send alerts', 'Recent alerts']],
    ['/apps/new', '18-new-app', ['What are you deploying?', 'Laravel', 'Plain PHP']],
    ['/db/mysql/shop_prod', '15-db-browser', ['orders', 'SQL']],
    ['/db/mysql/shop_prod?t=orders', '16-db-rows', ['orders', '11.50', 'Insert row']],
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

  console.log('\n▶ Old /jobs link')
  await go('/jobs', 2500)
  expect((await ev('return location.pathname')) === '/activity', '/jobs redirects to /activity')
  expect(/Activity/.test(await ev(`return [...document.querySelectorAll('.nav-item')].map(a=>a.textContent).join('|')`)) && !/\bJobs\b/.test(await ev(`return [...document.querySelectorAll('.nav-item')].map(a=>a.textContent).join('|')`)), 'nav shows Activity instead of Jobs')

  console.log('\n▶ Services: version on one line')
  await go('/services', 3000)
  const wraps = await ev(`return [...document.querySelectorAll('.svc-meta')].filter(e => e.getClientRects().length > 1 || e.scrollHeight > e.clientHeight + 2).map(e => e.textContent)`)
  expect(Array.isArray(wraps) && wraps.length === 0, `unit · version never wraps${wraps?.length ? ': ' + wraps.join(', ') : ''}`)

  console.log('\n▶ Restart a service from the UI (sudo prompt)')
  await ev(`document.querySelector('[aria-label="Restart Nginx"]').click(); return 1`)
  await sleep(400)
  expect(!!(await ev(`return document.querySelector('.confirm-ask')?.innerText`)), 'inline confirm appears (no browser dialog)')
  await ev(`document.querySelector('.confirm-ask .btn-danger').click(); return 1`)
  await sleep(1500)
  expect(!!(await ev(`return document.querySelector('.sudo-prompt input[type=password]') ? 1 : 0`)), 'sudo prompt asks for the password inline')
  await shot('11-sudo-prompt')
  await ev(`const i=document.querySelector('#sudo-pw'); i.value='wrong-password'; i.dispatchEvent(new Event('input')); i.form.requestSubmit(); return 1`)
  await sleep(1500)
  expect(/wrong password/i.test(await ev(`return document.querySelector('.sudo-prompt')?.innerText || ''`)), 'wrong sudo password shows an error, prompt stays')
  await ev(`const i=document.querySelector('#sudo-pw'); i.value=${JSON.stringify(PASSWORD)}; i.dispatchEvent(new Event('input')); i.form.requestSubmit(); return 1`)
  let restarted = false
  for (let i = 0; i < 15 && !restarted; i++) { await sleep(500); restarted = /Nginx restarted/i.test(await text()) }
  expect(restarted, 'after the password, the restart is retried automatically')
  expect(!(await ev(`return document.querySelector('.sudo-prompt') ? 1 : 0`)), 'sudo prompt closes')

  console.log('\n▶ Security: one-click fix (Preview → Apply → Rollback)')
  await go('/security', 3000)
  await ev(`[...document.querySelectorAll('section[aria-labelledby="fix-nginx-h"] button')].find(b=>b.textContent.trim()==='Preview')?.click(); return 1`)
  let pv = ''
  for (let i = 0; i < 20; i++) { await sleep(500); pv = await ev(`return document.querySelector('.change-preview')?.innerText || ''`); if (pv) break }
  expect(/This will change 1 file and run 2 commands/.test(pv), 'preview: "This will change 1 file and run 2 commands"')
  expect(!!(await ev(`return [...document.querySelectorAll('.change-preview .dl.add')].some(l => l.textContent.includes('server_tokens off;')) ? 1 : 0`)), 'diff shows +server_tokens off; as an added line')
  await shot('12-security-preview')
  await ev(`[...document.querySelectorAll('.change-preview button')].find(b=>b.textContent.trim()==='Apply')?.click(); return 1`)
  let done = ''
  for (let i = 0; i < 40; i++) { await sleep(500); done = await text(); if (/nginx no longer shows its version|reverted|did not apply/i.test(done)) break }
  expect(/nginx no longer shows its version/i.test(done), 'apply finishes: "nginx no longer shows its version"')
  expect(/Applied/.test(done), 'result pill says Applied')
  await shot('13-security-applied')
  const rbHref = await ev(`return [...document.querySelectorAll('a')].find(a=>a.textContent.trim()==='Rollback')?.getAttribute('href') || ''`)
  expect(/^\/activity\?open=/.test(rbHref), 'result has a Rollback link to Activity')
  await go(rbHref, 3500)
  const body = await ev(`return document.querySelector('.act.open .act-body')?.innerText || ''`)
  expect(/zz-nexacloud\.conf/.test(body) && /nginx -t/.test(body), 'Activity opens the change with its files and commands')
  expect(/Started: Hide nginx version|Snapshot saved/.test(body), 'Activity shows the job output')
  await shot('14-activity-change')
  await ev(`document.querySelector('.act.open .act-head').click(); return 1`)
  await sleep(6500)
  expect(!(await ev(`return document.querySelector('.act.open') ? 1 : 0`)), 'a collapsed deep-linked item stays collapsed after the next refresh')
  await ev(`document.querySelector('.act[data-id="${rbHref.split('open=')[1]}"] .act-head').click(); return 1`)
  await sleep(1500)
  const cid = rbHref.split('open=')[1]
  const countRollbacks = `return [...document.querySelectorAll('.act-title')].filter(t => t.textContent.includes('Roll back: Hide nginx version')).length`
  const before = await ev(countRollbacks)
  await ev(`document.querySelector('.act.open [aria-label^="Roll back"]').click(); return 1`)
  await sleep(400)
  expect(!!(await ev(`return document.querySelector('.act.open .confirm-ask')?.innerText`)), 'rollback asks for inline confirmation')
  await ev(`document.querySelector('.act.open .confirm-ask .btn-danger').click(); return 1`)
  let rolled = false
  for (let i = 0; i < 30 && !rolled; i++) {
    await sleep(500)
    const pill = await ev(`return document.querySelector('.act[data-id="${cid}"] .pill')?.textContent || ''`)
    rolled = pill.trim() === 'Rolled back' && (await ev(countRollbacks)) === before + 1
  }
  expect(rolled, 'rollback runs: the change shows "Rolled back" and a new rollback entry appears')
  await shot('15-activity-rolled-back')

  console.log('\n▶ Phone width')
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true })
  await go('/', 3000)
  const overflow = await ev('return document.documentElement.scrollWidth > window.innerWidth + 1')
  expect(!overflow, 'dashboard has no horizontal scroll at 390 px')
  await shot('10-dashboard-mobile')
  await go('/activity', 3000)
  expect(!(await ev('return document.documentElement.scrollWidth > window.innerWidth + 1')), 'activity has no horizontal scroll at 390 px')
  await shot('16-activity-mobile')
  await go('/security', 3000)
  expect(!(await ev('return document.documentElement.scrollWidth > window.innerWidth + 1')), 'security has no horizontal scroll at 390 px')
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
