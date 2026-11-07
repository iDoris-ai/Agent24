// KB-C2-03 (#1622) smoke: drives the real KnowledgeViewHost in Electron
// against two loopback servers — the allowed origin (A) and a foreign one (B)
// — and checks the exit criteria of #1002 that only a real Chromium can show,
// plus the PR-Daemon #1630 probes (TURN over TCP, preconnect, ws, 302):
// no Node/preload/host API in the page, popups/foreign requests/navigation/
// frames/workers/Service Workers/downloads/permissions refused, and two
// instances (two profiles) whose partitions see nothing of each other, the
// first one cleared once it is destroyed.
//
//   pnpm exec tsc -p tsconfig.electron.json
//   pnpm exec electron scripts/knowledge-view-smoke.cjs
//
// Prints one line per check and exits non-zero if any fails.

const http = require('node:http')
const net = require('node:net')
const path = require('node:path')
const { app, BrowserWindow, WebContentsView, session } = require('electron')
const { KnowledgeViewHost } = require(path.join(__dirname, '..', 'dist', 'main', 'knowledge-view.js'))

const hits = { A: [], B: [] }
/** TCP connections opened to the foreign server B and to the fake TURN port. */
const connections = { B: 0, TURN: 0 }

const PROBE = `
(async () => {
  const r = {}
  r.before = { local: localStorage.getItem('k'), cookie: document.cookie }
  r.node = typeof require + '/' + typeof process + '/' + typeof module
  r.hostApi = typeof window.agent24
  r.popup = window.open(B + '/popup') === null ? 'null' : 'window'
  try { await fetch(B + '/fetch'); r.foreignFetch = 'ok' } catch { r.foreignFetch = 'blocked' }
  await new Promise((done) => { const i = new Image(); i.onload = () => { r.foreignImg = 'loaded'; done() }; i.onerror = () => { r.foreignImg = 'blocked'; done() }; i.src = B + '/img' })
  const f = document.createElement('iframe'); f.src = '/frame'; document.body.appendChild(f)
  try { new Worker('/worker.js'); r.worker = 'created' } catch { r.worker = 'blocked' }
  try { await navigator.serviceWorker.register('/sw.js'); r.serviceWorker = 'registered' } catch { r.serviceWorker = 'blocked' }
  r.notification = await Notification.requestPermission()
  r.geo = await new Promise((done) => navigator.geolocation.getCurrentPosition(() => done('granted'), (e) => done('denied:' + e.code)))
  try { await navigator.mediaDevices.getUserMedia({ audio: true }); r.media = 'granted' } catch (e) { r.media = 'denied' }
  try { eval('1'); r.eval = 'ran' } catch { r.eval = 'blocked' }
  const a = document.createElement('a'); a.href = '/download'; a.download = 'x.bin'; document.body.appendChild(a); a.click()
  // PR-Daemon #1630 C1/R2-6: ways out that bypass webRequest or CSP in some engines.
  try {
    const pc = new RTCPeerConnection({ iceServers: [{ urls: 'turn:' + TURN + '?transport=tcp', username: 'u', credential: 'p' }] })
    pc.createDataChannel('x')
    await pc.setLocalDescription(await pc.createOffer())
    await new Promise((d) => setTimeout(d, 2000))
    pc.close()
    r.turn = 'ran'
  } catch { r.turn = 'blocked' }
  // preconnect opens a TCP connection without a request, so it is seen as a
  // connection to B. (dns-prefetch is not probed: with an IP literal there is
  // no lookup, and observing DNS would need a resolver of our own.)
  { const l = document.createElement('link'); l.rel = 'preconnect'; l.href = B; document.head.appendChild(l) }
  r.ws = await new Promise((d) => {
    try {
      const w = new WebSocket(B.replace(/^http/, 'ws') + '/ws')
      w.onopen = () => d('open'); w.onerror = () => d('blocked'); setTimeout(() => d('timeout'), 1500)
    } catch { d('blocked') }
  })
  try { const res = await fetch('/redir'); r.redirect = 'followed:' + res.status } catch { r.redirect = 'blocked' }
  localStorage.setItem('k', 'v-' + location.search); document.cookie = 'c=1; path=/'
  r.after = { local: localStorage.getItem('k'), cookie: document.cookie }
  window.__probe = r
})()
`

function server(name, handler) {
  return new Promise((resolve) => {
    const s = http.createServer((req, res) => { hits[name].push(req.url); handler(req, res) })
    s.listen(0, '127.0.0.1', () => resolve({ s, origin: `http://127.0.0.1:${s.address().port}` }))
  })
}

let originB = ''
let turnAddress = ''
async function main() {
  const b = await server('B', (_req, res) => { res.writeHead(200, { 'content-type': 'text/plain' }); res.end('B') })
  b.s.on('connection', () => { connections.B++ })
  originB = b.origin
  const turn = await new Promise((resolve) => {
    const t = net.createServer((socket) => { connections.TURN++; socket.destroy() })
    t.listen(0, '127.0.0.1', () => resolve(t))
  })
  turnAddress = `127.0.0.1:${turn.address().port}`
  const a = await server('A', (req, res) => {
    const url = new URL(req.url, 'http://x')
    if (url.pathname === '/probe.js') {
      res.writeHead(200, { 'content-type': 'text/javascript' })
      return res.end(`const B = ${JSON.stringify(originB)}; const TURN = ${JSON.stringify(turnAddress)};\n${PROBE}`)
    }
    if (url.pathname === '/redir') {
      res.writeHead(302, { location: `${originB}/redir-target` })
      return res.end()
    }
    if (url.pathname === '/download') {
      res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-disposition': 'attachment; filename=x.bin' })
      return res.end('xx')
    }
    if (url.pathname === '/worker.js' || url.pathname === '/sw.js') {
      res.writeHead(200, { 'content-type': 'text/javascript' })
      return res.end('')
    }
    res.writeHead(200, { 'content-type': 'text/html' })
    res.end('<!doctype html><html><body><p>kb</p><script src="/probe.js"></script></body></html>')
  })

  await app.whenReady()
  const win = new BrowserWindow({ show: false, width: 900, height: 700 })
  await win.loadURL('about:blank')
  let binding = 'profile-1'
  const partitions = []
  const downloads = []
  const host = new KnowledgeViewHost({
    window: () => win,
    createView: (webPreferences) => new WebContentsView({ webPreferences }),
    sessionFor: (partition) => {
      partitions.push(partition)
      const ses = session.fromPartition(partition, { cache: false })
      // Registered before the host's own handler, so it reads the outcome in a
      // microtask: still this tick (a prevented DownloadItem is gone on the next).
      ses.on('will-download', (event) => queueMicrotask(() => downloads.push(event.defaultPrevented ? 'prevented' : 'allowed')))
      return ses
    },
    target: () => ({ origin: a.origin, url: `${a.origin}/?${binding}` }),
    binding: () => binding,
    packaged: true,
    log: () => {},
  })

  const view = () => win.contentView.children.find((c) => c.webContents && c.webContents !== win.webContents)
  const probe = async () => {
    for (let i = 0; i < 100; i++) {
      const r = await view().webContents.executeJavaScript('window.__probe || null')
      if (r) return r
      await new Promise((d) => setTimeout(d, 100))
    }
    throw new Error('probe did not finish')
  }

  const checks = []
  const check = (name, ok, detail) => { checks.push(ok); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail === undefined ? '' : ` — ${JSON.stringify(detail)}`}`) }

  const first = await host.show({ x: 0, y: 0, width: 800, height: 600 }, 'dark')
  check('show opens in the same window', first.ok && win.contentView.children.length === 1 && BrowserWindow.getAllWindows().length === 1, first)
  const r1 = await probe()
  await new Promise((d) => setTimeout(d, 500))
  check('no Node in the page', r1.node === 'undefined/undefined/undefined', r1.node)
  check('no host API (no preload)', r1.hostApi === 'undefined', r1.hostApi)
  check('window.open refused, no new window', r1.popup === 'null' && BrowserWindow.getAllWindows().length === 1, r1.popup)
  check('foreign fetch and image blocked', r1.foreignFetch === 'blocked' && r1.foreignImg === 'blocked', [r1.foreignFetch, r1.foreignImg])
  check('foreign origin received nothing', hits.B.length === 0, hits.B)
  check('no frame loaded', !hits.A.includes('/frame'), hits.A)
  check('Worker and Service Worker refused', r1.worker === 'blocked' && r1.serviceWorker === 'blocked', [r1.worker, r1.serviceWorker])
  check('permissions denied', r1.notification === 'denied' && r1.geo.startsWith('denied') && r1.media === 'denied', [r1.notification, r1.geo, r1.media])
  check('inline eval blocked by the partition CSP', r1.eval === 'blocked', r1.eval)
  check('download prevented', downloads.length === 1 && downloads[0] === 'prevented', downloads)
  check('no TURN-over-TCP connection (contract decision #30)', connections.TURN === 0, { turn: r1.turn, connections: connections.TURN })
  // B is plain http and never upgrades, so 'open' cannot happen; what tells is that B saw no connection.
  check('WebSocket to a foreign host refused', r1.ws !== 'open' && connections.B === 0, { ws: r1.ws, connections: connections.B })
  check('302 to a foreign host not followed', r1.redirect === 'blocked' && !hits.B.includes('/redir-target'), r1.redirect)
  check('no TCP connection at all to the foreign host (preconnect, ws)', connections.B === 0, connections.B)

  const wc = view().webContents
  await wc.executeJavaScript(`location.href = ${JSON.stringify(`${originB}/nav`)}`)
  await new Promise((d) => setTimeout(d, 700))
  check('cross-origin navigation refused', wc.getURL().startsWith(a.origin) && !hits.B.includes('/nav'), wc.getURL())
  await wc.executeJavaScript(`location.href = 'file:///etc/hosts'`)
  await new Promise((d) => setTimeout(d, 500))
  check('file:// navigation refused', wc.getURL().startsWith(a.origin), wc.getURL())

  // A second profile: a new instance in a new partition that sees nothing of the first.
  const firstSession = session.fromPartition(partitions[0], { cache: false })
  const firstView = view()
  binding = 'profile-2'
  host.bindingChanged()
  // On screen, so the host replaces it with the new binding's view at once.
  check('binding change takes the first instance down', !win.contentView.children.includes(firstView) && win.contentView.children.length === 1)
  // Cleared once the page is destroyed (or after the host's bounded wait).
  await new Promise((d) => setTimeout(d, 2500))
  check('first page destroyed', wc.isDestroyed())
  const cookies1 = await firstSession.cookies.get({})
  check('first partition cleared', cookies1.length === 0, cookies1.length)
  const r2 = await probe()
  check('second profile uses a different non-persistent partition', partitions.length === 2 && partitions[0] !== partitions[1]
    && partitions.every((p) => !p.startsWith('persist:')), partitions)
  check('second profile sees none of the first one’s storage', r2.before.local === null && r2.before.cookie === '', r2.before)

  host.shutdown()
  check('quit destroys the view', win.contentView.children.length === 0)
  const again = await host.show({ x: 0, y: 0, width: 800, height: 600 }, 'dark')
  check('no new view after quit', !again.ok && win.contentView.children.length === 0, again)

  a.s.close(); b.s.close(); turn.close()
  const failed = checks.filter((ok) => !ok).length
  console.log(`${checks.length - failed}/${checks.length} PASS`)
  app.exit(failed === 0 ? 0 : 1)
}

main().catch((error) => { console.error(error); app.exit(2) })
