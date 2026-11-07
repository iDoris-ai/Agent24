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
// Prints one line per check (33) and exits non-zero if any fails.

const http = require('node:http')
const net = require('node:net')
const path = require('node:path')
const { app, BrowserWindow, WebContentsView, session } = require('electron')
const { KnowledgeViewHost } = require(path.join(__dirname, '..', 'dist', 'main', 'knowledge-view.js'))

const hits = { A: [], B: [] }
/** TCP connections opened to the foreign server B and to the fake TURN port. */
const connections = { B: 0, TURN: 0, ICE: 0 }

// Every step that waits on the browser is bounded and named, so a step that
// never answers on a real machine (a permission or device prompt, say) shows
// up as 'timeout' in its check and as the last stage, instead of the whole
// probe going silent (David's first run on a Mac, 2026-11-08).
const PROBE = `
const stage = (name) => { window.__stage = name }
const timedOut = []
// Records the step only when its own timer wins, and clears that timer when the
// step settles first, so a slow probe never blames a step that answered.
const within = (p, ms) => {
  const name = window.__stage
  let timer
  const late = new Promise((d) => { timer = setTimeout(() => { timedOut.push(name); d('timeout') }, ms) })
  return Promise.race([Promise.resolve(p).finally(() => clearTimeout(timer)), late])
}
;(async () => {
  const r = {}
  try {
    stage('storage-before'); r.before = { local: localStorage.getItem('k'), cookie: document.cookie }
    r.node = typeof require + '/' + typeof process + '/' + typeof module
    r.hostApi = typeof window.agent24
    stage('popup'); r.popup = window.open(B + '/popup') === null ? 'null' : 'window'
    stage('foreign-fetch'); r.foreignFetch = await within(fetch(B + '/fetch').then(() => 'ok', () => 'blocked'), 3000)
    stage('foreign-img'); r.foreignImg = await within(new Promise((done) => { const i = new Image(); i.onload = () => done('loaded'); i.onerror = () => done('blocked'); i.src = B + '/img' }), 3000)
    // Refused by the partition CSP (frame-src 'none'): Electron logs
    // ERR_BLOCKED_BY_CSP for /frame, which is the expected outcome.
    stage('frame'); { const f = document.createElement('iframe'); f.src = '/frame'; document.body.appendChild(f) }
    stage('worker'); try { new Worker('/worker.js'); r.worker = 'created' } catch { r.worker = 'blocked' }
    stage('service-worker'); r.serviceWorker = await within(Promise.resolve().then(() => navigator.serviceWorker.register('/sw.js')).then(() => 'registered', () => 'blocked'), 3000)
    stage('notification'); r.notification = await within(Promise.resolve().then(() => Notification.requestPermission()).catch((e) => 'threw:' + e.name), 3000)
    stage('geolocation'); r.geo = await within(new Promise((done) => navigator.geolocation.getCurrentPosition(() => done('granted'), (e) => done('denied:' + e.code), { timeout: 2000 })), 3000)
    stage('media'); r.media = await within(Promise.resolve().then(() => navigator.mediaDevices.getUserMedia({ audio: true })).then(() => 'granted', () => 'denied'), 3000)
    stage('eval'); try { eval('1'); r.eval = 'ran' } catch { r.eval = 'blocked' }
    stage('download'); { const a = document.createElement('a'); a.href = '/download'; a.download = 'x.bin'; document.body.appendChild(a); a.click() }
    // PR-Daemon #1630 C1/R2-6: ways out that bypass webRequest or CSP in some engines.
    // 'unavailable': no RTCPeerConnection at all (a pass); 'rejected': the API
    // threw before ICE was tried (proves nothing, so it fails).
    stage('turn')
    if (typeof RTCPeerConnection === 'undefined') r.turn = 'unavailable'
    else try {
      const pc = new RTCPeerConnection({ iceServers: [{ urls: 'turn:' + TURN + '?transport=tcp', username: 'u', credential: 'p' }] })
      pc.createDataChannel('x')
      const offer = await within(pc.createOffer(), 3000)
      const local = offer === 'timeout' ? 'timeout' : await within(pc.setLocalDescription(offer).then(() => 'set'), 3000)
      await new Promise((d) => setTimeout(d, 2000))
      pc.close()
      r.turn = offer === 'timeout' || local === 'timeout' ? 'timeout' : 'ran'
    } catch { r.turn = 'rejected' }
    // ICE-TCP active (decision #30): two local peers agree on a session in the
    // page, then one is handed a passive TCP candidate at the ICE port, so it
    // tries an outgoing TCP connection there. Same socket path as TURN-TCP.
    stage('ice-tcp')
    if (typeof RTCPeerConnection === 'undefined') r.iceTcp = 'unavailable'
    else try {
      const a = new RTCPeerConnection(); const b = new RTCPeerConnection()
      // Local candidates of the side that dials out: with connections 0, a
      // TCP-active one gathered means the proxy held it; none gathered means
      // this row proves nothing (QA reads the row by this field).
      const gathered = { total: 0, tcpActive: 0, tcpOther: 0 }
      a.onicecandidate = (e) => {
        const c = e.candidate
        if (!c) return
        gathered.total++
        if (c.protocol === 'tcp') { if (c.tcpType === 'active') gathered.tcpActive++; else gathered.tcpOther++ }
      }
      a.createDataChannel('x')
      const offer = await within(a.createOffer(), 3000)
      if (offer === 'timeout') throw new Error('timeout')
      await within(a.setLocalDescription(offer), 3000)
      await within(b.setRemoteDescription(offer), 3000)
      const answer = await within(b.createAnswer(), 3000)
      if (answer === 'timeout') throw new Error('timeout')
      await within(b.setLocalDescription(answer), 3000)
      await within(a.setRemoteDescription(answer), 3000)
      const [host, port] = ICE.split(':')
      await within(a.addIceCandidate({ candidate: 'candidate:1 1 tcp 1518280447 ' + host + ' ' + port + ' typ host tcptype passive', sdpMid: '0', sdpMLineIndex: 0 }), 3000)
      await new Promise((d) => setTimeout(d, 2000))
      // Read alongside the count: if Chromium never paired the candidate (no
      // local TCP-active candidate), connections 0 says nothing for this row.
      r.iceState = a.iceConnectionState
      r.iceLocal = gathered
      const stats = await within(a.getStats(), 3000)
      if (stats !== 'timeout') {
        let n = 0
        stats.forEach((v) => { if (v.type === 'local-candidate' && v.protocol === 'tcp' && v.tcpType === 'active') n++ })
        r.iceLocal.statsTcpActive = n
      }
      a.close(); b.close()
      r.iceTcp = timedOut.includes('ice-tcp') ? 'timeout' : 'ran'
    } catch (e) { r.iceTcp = timedOut.includes('ice-tcp') ? 'timeout' : 'rejected:' + (e && e.name) }
    // preconnect opens a TCP connection without a request, so it is seen as a
    // connection to B. (dns-prefetch is not probed: with an IP literal there is
    // no lookup, and observing DNS would need a resolver of our own.)
    stage('preconnect'); { const l = document.createElement('link'); l.rel = 'preconnect'; l.href = B; document.head.appendChild(l) }
    stage('ws')
    r.ws = await new Promise((d) => {
      try {
        const w = new WebSocket(B.replace(/^http/, 'ws') + '/ws')
        w.onopen = () => d('open'); w.onerror = () => d('blocked'); setTimeout(() => d('timeout'), 1500)
      } catch { d('blocked') }
    })
    stage('redirect'); r.redirect = await within(fetch('/redir').then((res) => 'followed:' + res.status, () => 'blocked'), 3000)
    stage('storage-after'); localStorage.setItem('k', 'v-' + location.search); document.cookie = 'c=1; path=/'
    r.after = { local: localStorage.getItem('k'), cookie: document.cookie }
    stage('done')
  } catch (e) {
    r.error = String(e)
  }
  r.stage = window.__stage
  r.timedOut = timedOut.slice()
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
let iceAddress = ''
async function main() {
  const b = await server('B', (_req, res) => { res.writeHead(200, { 'content-type': 'text/plain' }); res.end('B') })
  b.s.on('connection', () => { connections.B++ })
  originB = b.origin
  const turn = await new Promise((resolve) => {
    const t = net.createServer((socket) => { connections.TURN++; socket.destroy() })
    t.listen(0, '127.0.0.1', () => resolve(t))
  })
  turnAddress = `127.0.0.1:${turn.address().port}`
  const ice = await new Promise((resolve) => {
    const t = net.createServer((socket) => { connections.ICE++; socket.destroy() })
    t.listen(0, '127.0.0.1', () => resolve(t))
  })
  iceAddress = `127.0.0.1:${ice.address().port}`
  const a = await server('A', (req, res) => {
    const url = new URL(req.url, 'http://x')
    if (url.pathname === '/probe.js') {
      res.writeHead(200, { 'content-type': 'text/javascript' })
      return res.end(`const B = ${JSON.stringify(originB)}; const TURN = ${JSON.stringify(turnAddress)}; const ICE = ${JSON.stringify(iceAddress)};\n${PROBE}`)
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
  const CONTROL = 'profile-control'
  const controlAsked = []
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
      // The control profile (see the end): the host still asks for its dead
      // proxy, this script puts the partition on direct instead.
      if (binding === CONTROL) {
        const set = ses.setProxy.bind(ses)
        ses.setProxy = (config) => { controlAsked.push(config); return set({ mode: 'direct' }) }
      }
      return ses
    },
    target: () => ({ origin: a.origin, url: `${a.origin}/?${binding}` }),
    binding: () => binding,
    packaged: true,
    log: () => {},
  })

  const view = () => win.contentView.children.find((c) => c.webContents && c.webContents !== win.webContents)
  const probe = async () => {
    // The probe's own steps are bounded (about 55 s at worst, ICE-TCP included); past 70 s it
    // is reported as unfinished with the stage it stopped at, and the run goes on.
    // executeJavaScript itself is raced against the deadline too: a renderer
    // that stops answering must not hold the run.
    const deadline = Date.now() + 70_000
    const ask = (code) => Promise.race([
      view().webContents.executeJavaScript(code),
      new Promise((d) => setTimeout(() => d(undefined), Math.max(0, deadline - Date.now()))),
    ])
    let stage = null
    while (Date.now() < deadline) {
      const r = await ask('window.__probe || null')
      if (r) return r
      const s = await ask('window.__stage || null')
      if (s) stage = s
      await new Promise((d) => setTimeout(d, 100))
    }
    return { unfinished: true, stage }
  }

  const checks = []
  const check = (name, ok, detail) => { checks.push(ok); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail === undefined ? '' : ` — ${JSON.stringify(detail)}`}`) }

  const first = await host.show({ x: 0, y: 0, width: 800, height: 600 }, 'dark')
  check('show opens in the same window', first.ok && win.contentView.children.length === 1 && BrowserWindow.getAllWindows().length === 1, first)
  const r1 = await probe()
  check('probe finished', r1.stage === 'done' && !r1.error && !r1.unfinished && r1.timedOut?.length === 0, { stage: r1.stage, error: r1.error, unfinished: r1.unfinished, timedOut: r1.timedOut })
  await new Promise((d) => setTimeout(d, 500))
  check('no Node in the page', r1.node === 'undefined/undefined/undefined', r1.node)
  check('no host API (no preload)', r1.hostApi === 'undefined', r1.hostApi)
  check('window.open refused, no new window', r1.popup === 'null' && BrowserWindow.getAllWindows().length === 1, r1.popup)
  // The foreign server B is on loopback too, so from here on the dead proxy
  // stands in front of it as well: these rows show the layers together. The
  // control profile at the end shows webRequest and the CSP alone (#1643 PR-Daemon A).
  check('foreign fetch and image blocked', r1.foreignFetch === 'blocked' && r1.foreignImg === 'blocked', [r1.foreignFetch, r1.foreignImg])
  check('foreign origin received nothing', hits.B.length === 0, hits.B)
  check('no frame loaded', !hits.A.includes('/frame'), hits.A)
  check('Worker and Service Worker refused', r1.worker === 'blocked' && r1.serviceWorker === 'blocked', [r1.worker, r1.serviceWorker])
  check('permissions denied', r1.notification === 'denied' && r1.geo === 'denied:1' && r1.media === 'denied', [r1.notification, r1.geo, r1.media])
  check('inline eval blocked by the partition CSP', r1.eval === 'blocked', r1.eval)
  check('download prevented', downloads.length === 1 && downloads[0] === 'prevented', downloads)
  // 'ran' means an offer was made and set, so ICE gathering was really tried; a
  // timeout or a rejection there proves nothing. 'unavailable' (no RTCPeerConnection) is a pass.
  const triedOrAbsent = (v) => v === 'ran' || v === 'unavailable'
  check('no TURN-over-TCP connection (contract decision #30)', triedOrAbsent(r1.turn) && connections.TURN === 0, { turn: r1.turn, connections: connections.TURN })
  // PASS/FAIL stays on the count; 'unproven' flags a 0 that no TCP-active
  // candidate stood behind, so the PASS is not read as "blocked".
  const iceEvidence = (r, added) => ({
    ...(added === 0 && r.iceTcp === 'ran' && !(r.iceLocal?.tcpActive > 0) && !(r.iceLocal?.statsTcpActive > 0)
      ? { unproven: 'no TCP-active local candidate was gathered' } : {}),
    iceTcp: r.iceTcp, iceState: r.iceState, local: r.iceLocal, connections: added,
  })
  check('no ICE-TCP active connection (contract decision #30)', triedOrAbsent(r1.iceTcp) && connections.ICE === 0, iceEvidence(r1, connections.ICE))
  const webrtcBefore = { TURN: connections.TURN, ICE: connections.ICE }
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
  check('second probe finished', r2.stage === 'done' && !r2.error && !r2.unfinished && r2.timedOut?.length === 0, { stage: r2.stage, error: r2.error, unfinished: r2.unfinished, timedOut: r2.timedOut })
  check('second profile uses a different non-persistent partition', partitions.length === 2 && partitions[0] !== partitions[1]
    && partitions.every((p) => !p.startsWith('persist:')), partitions)
  check('second profile sees none of the first one’s storage', r2.before?.local === null && r2.before?.cookie === '', r2.before)
  // The new partition must get the dead proxy too, not only the first one.
  check('second profile: no TURN- or ICE-TCP connection either (decision #30)',
    triedOrAbsent(r2.turn) && triedOrAbsent(r2.iceTcp) && connections.TURN === webrtcBefore.TURN && connections.ICE === webrtcBefore.ICE,
    { turn: r2.turn, newTurn: connections.TURN - webrtcBefore.TURN, ice: iceEvidence(r2, connections.ICE - webrtcBefore.ICE) })

  // Positive control (#1643 PR-Daemon A): a third profile whose partition is
  // put on direct by this script; the host's CSP, webRequest and permission
  // refusals stay as they are. The foreign host must still get nothing (that
  // rests on webRequest and the CSP, not on the proxy), and TURN/ICE-TCP must
  // now reach their ports: only then do the 0s above mean "the dead proxy held"
  // rather than "this machine never tries".
  const base = { TURN: connections.TURN, ICE: connections.ICE, B: connections.B, hitsB: hits.B.length }
  binding = CONTROL
  host.bindingChanged()
  const rc = await probe()
  check('control probe finished', rc.stage === 'done' && !rc.error && !rc.unfinished && rc.timedOut?.length === 0, { stage: rc.stage, error: rc.error, unfinished: rc.unfinished, timedOut: rc.timedOut })
  await new Promise((d) => setTimeout(d, 500))
  const viewOrigin = new URL(a.origin)
  check('control: the host still asked for its dead proxy', controlAsked.length === 1
    && JSON.stringify(controlAsked[0]).includes(`<-loopback>;${viewOrigin.host}`), controlAsked)
  check('control: foreign host refused by webRequest and the CSP alone (no dead proxy)',
    rc.foreignFetch === 'blocked' && rc.foreignImg === 'blocked' && rc.ws !== 'open' && rc.redirect === 'blocked'
      && connections.B === base.B && hits.B.length === base.hitsB,
    { fetch: rc.foreignFetch, img: rc.foreignImg, ws: rc.ws, redirect: rc.redirect, newConnections: connections.B - base.B, newHits: hits.B.slice(base.hitsB) })
  const controlTurn = connections.TURN - base.TURN
  const controlIce = connections.ICE - base.ICE
  check('control: TURN-over-TCP gets out without the dead proxy (so the 0 above is the proxy)', rc.turn === 'ran' && controlTurn >= 1, { turn: rc.turn, connections: controlTurn })
  // ICE-TCP is conclusive when the control connects, or when the dead-proxy
  // run had a TCP-active candidate to send and still made no connection.
  const iceConclusive = controlIce >= 1 || r1.iceLocal?.tcpActive > 0 || r1.iceLocal?.statsTcpActive > 0
  check('control: the ICE-TCP 0 above is conclusive', rc.iceTcp === 'ran' && iceConclusive,
    { ...(iceConclusive ? {} : { inconclusive: 'no connection without the proxy and no TCP-active candidate with it' }), control: iceEvidence(rc, controlIce), profile1Local: r1.iceLocal })

  host.shutdown()
  check('quit destroys the view', win.contentView.children.length === 0)
  const again = await host.show({ x: 0, y: 0, width: 800, height: 600 }, 'dark')
  check('no new view after quit', !again.ok && win.contentView.children.length === 0, again)

  a.s.close(); b.s.close(); turn.close(); ice.close()
  const failed = checks.filter((ok) => !ok).length
  console.log(`${checks.length - failed}/${checks.length} PASS`)
  app.exit(failed === 0 ? 0 : 1)
}

main().catch((error) => { console.error(error); app.exit(2) })
