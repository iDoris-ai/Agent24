const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

async function listTargets() {
  const response = await fetch('http://127.0.0.1:9222/json/list')
  if (!response.ok) throw new Error('CDP list HTTP ' + response.status)
  return response.json()
}

async function evaluate(target, expression) {
  const ws = new WebSocket(target.webSocketDebuggerUrl)
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, { once: true })
    ws.addEventListener('error', reject, { once: true })
  })
  const id = 1
  ws.send(JSON.stringify({
    id,
    method: 'Runtime.evaluate',
    params: { expression, returnByValue: true, awaitPromise: true },
  }))
  const result = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('CDP evaluate timeout')), 15_000)
    ws.addEventListener('message', (event) => {
      const message = JSON.parse(String(event.data))
      if (message.id === id) {
        clearTimeout(timer)
        resolve(message)
      }
    })
    ws.addEventListener('error', reject, { once: true })
  })
  ws.close()
  if (result.error) throw new Error(JSON.stringify(result.error))
  if (result.result?.exceptionDetails) {
    throw new Error('CDP evaluation failed: ' + JSON.stringify(result.result.exceptionDetails))
  }
  return result.result?.result?.value
}

let targets = await listTargets()
const shell = targets.find((target) =>
  target.type === 'page'
  && !target.url.startsWith('devtools://')
  && !/^http:\/\/127\.0\.0\.1:\d+\//.test(target.url)
)
if (!shell) throw new Error('Agent24 shell target not found: ' + JSON.stringify(targets))

const showResult = await evaluate(
  shell,
  'window.agent24.creativeShow({ x: 24, y: 96, width: 900, height: 640 })',
)
if (!showResult?.ok) {
  throw new Error('Creative show failed: ' + JSON.stringify(showResult))
}

let creative
for (let attempt = 0; attempt < 120; attempt += 1) {
  targets = await listTargets()
  creative = targets.find((target) =>
    target.type === 'page'
    && /^http:\/\/127\.0\.0\.1:\d+\//.test(target.url)
  )
  if (creative) break
  await sleep(1_000)
}
if (!creative) {
  throw new Error('Creative WebContentsView target never appeared: ' + JSON.stringify(targets))
}

const origin = new URL(creative.url).origin
const ready = await fetch(origin + '/api/ready')
if (!ready.ok) throw new Error('Open Design /api/ready HTTP ' + ready.status)
const readyBody = await ready.text()
const documentState = await evaluate(creative, 'document.readyState')
if (!['interactive', 'complete'].includes(documentState)) {
  throw new Error('Creative document not rendered: ' + documentState)
}

console.log(JSON.stringify({
  showResult,
  creativeUrl: creative.url,
  origin,
  readyBody,
  documentState,
}, null, 2))
