import { useState, useEffect, useRef } from 'react'
import { useT } from '../i18n'
import { codeSandboxMessages } from './CodeSandbox.i18n'

interface StatusResult {
  available: boolean
  error?: string
}

interface RunResult {
  ok: boolean
  output?: string
  error?: string
}

const PLACEHOLDER = `import sys
print(f"Python {sys.version}")
print("Hello from BoxLite sandbox!")
`

function UnsupportedScreen({ error }: { error: string }) {
  const t = useT(codeSandboxMessages)
  return (
    <div className="content" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
      <div style={{ textAlign: 'center', maxWidth: 420 }}>
        <div style={{ fontSize: 48, marginBottom: 16 }}>🖥️</div>
        <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 8 }}>{t('unsupported.title')}</div>
        <div style={{ fontSize: 13, color: 'var(--muted)', lineHeight: 1.7, marginBottom: 20 }}>
          {t('unsupported.body')}
        </div>
        <div style={{
          fontSize: 11,
          fontFamily: 'monospace',
          color: '#e57373',
          background: 'var(--surface2)',
          border: '1px solid var(--border)',
          borderRadius: 8,
          padding: '10px 14px',
          textAlign: 'left',
          wordBreak: 'break-all',
        }}>
          {error}
        </div>
      </div>
    </div>
  )
}

export default function CodeSandboxPage() {
  const t = useT(codeSandboxMessages)
  const [code, setCode] = useState(PLACEHOLDER)
  const [output, setOutput] = useState('')
  const [outputIsError, setOutputIsError] = useState(false)
  const [running, setRunning] = useState(false)
  const [status, setStatus] = useState<StatusResult | null>(null)
  const abortRef = useRef(false)

  useEffect(() => {
    void window.agent24.backendProxy({ method: 'GET', path: '/api/codebox/status' })
      .then((res) => {
        if (res.ok) setStatus(res.data as StatusResult)
      })
      .catch(() => setStatus({ available: false, error: t('unsupported.backendUnavailable') }))
  }, [])

  // Not yet loaded — blank while checking
  if (status === null) return <div className="content" />

  // Hardware not supported — full-page notice, no degraded editor
  if (!status.available) return <UnsupportedScreen error={status.error ?? t('unsupported.unknown')} />

  async function run() {
    if (running) return
    setRunning(true)
    setOutputIsError(false)
    setOutput(t('starting'))
    abortRef.current = false
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/codebox/run',
        body: { code },
      })
      if (abortRef.current) return
      const data = res.data as RunResult
      setOutputIsError(!data.ok)
      setOutput(data.ok ? (data.output ?? t('noOutput')) : t('error', { msg: data.error ?? t('unknownError') }))
    } catch (err) {
      if (!abortRef.current) {
        setOutputIsError(true)
        setOutput(t('error', { msg: String(err) }))
      }
    } finally {
      setRunning(false)
    }
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">{t('title')}</div>
        <button
          className="btn btn-primary"
          style={{ fontSize: 12 }}
          onClick={() => void run()}
          disabled={running}
        >
          {running ? t('running') : t('run')}
        </button>
      </div>
      <div className="page-sub">{t('sub')}</div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, flex: 1 }}>
        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            {t('code')}
          </div>
          <textarea
            value={code}
            onChange={(e) => setCode(e.target.value)}
            spellCheck={false}
            style={{
              width: '100%',
              minHeight: 200,
              fontFamily: 'monospace',
              fontSize: 13,
              lineHeight: 1.6,
              padding: '10px 12px',
              background: 'var(--surface2)',
              color: 'var(--text)',
              border: '1px solid var(--border)',
              borderRadius: 8,
              resize: 'vertical',
              boxSizing: 'border-box',
            }}
          />
        </div>

        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            {t('output')}
          </div>
          <pre
            style={{
              minHeight: 100,
              padding: '10px 12px',
              background: 'var(--surface2)',
              border: '1px solid var(--border)',
              borderRadius: 8,
              fontFamily: 'monospace',
              fontSize: 12,
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-all',
              color: outputIsError ? '#e57373' : 'var(--text)',
              margin: 0,
            }}
          >
            {output || t('outputHint')}
          </pre>
        </div>
      </div>
    </div>
  )
}
