import { useState, useEffect, useRef } from 'react'

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
  return (
    <div className="content" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
      <div style={{ textAlign: 'center', maxWidth: 420 }}>
        <div style={{ fontSize: 48, marginBottom: 16 }}>🖥️</div>
        <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 8 }}>此设备不支持 Python 沙箱</div>
        <div style={{ fontSize: 13, color: 'var(--muted)', lineHeight: 1.7, marginBottom: 20 }}>
          Python 沙箱需要 Apple Silicon（M1/M2/M3）macOS 12+ 的 Hypervisor.framework，
          或 Linux x86_64/ARM64 with KVM。
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
  const [code, setCode] = useState(PLACEHOLDER)
  const [output, setOutput] = useState('')
  const [running, setRunning] = useState(false)
  const [status, setStatus] = useState<StatusResult | null>(null)
  const abortRef = useRef(false)

  useEffect(() => {
    void window.agent24.backendProxy({ method: 'GET', path: '/api/codebox/status' })
      .then((res) => {
        if (res.ok) setStatus(res.data as StatusResult)
      })
      .catch(() => setStatus({ available: false, error: 'Backend unavailable' }))
  }, [])

  // Not yet loaded — blank while checking
  if (status === null) return <div className="content" />

  // Hardware not supported — full-page notice, no degraded editor
  if (!status.available) return <UnsupportedScreen error={status.error ?? '未知错误'} />

  async function run() {
    if (running) return
    setRunning(true)
    setOutput('启动沙箱容器… (首次运行需拉取 python:slim 镜像，约需 1-2 分钟)')
    abortRef.current = false
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/codebox/run',
        body: { code },
      })
      if (abortRef.current) return
      const data = res.data as RunResult
      setOutput(data.ok ? (data.output ?? '(no output)') : `Error: ${data.error ?? 'unknown'}`)
    } catch (err) {
      if (!abortRef.current) setOutput(`Error: ${String(err)}`)
    } finally {
      setRunning(false)
    }
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">Python 沙箱</div>
        <button
          className="btn btn-primary"
          style={{ fontSize: 12 }}
          onClick={() => void run()}
          disabled={running}
        >
          {running ? '运行中…' : '▶ 运行'}
        </button>
      </div>
      <div className="page-sub">BoxLite 硬件级 VM 隔离 · 每次运行独立容器 · Apple Silicon Hypervisor.framework</div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, flex: 1 }}>
        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            代码
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
            输出
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
              color: output.startsWith('Error:') ? '#e57373' : 'var(--text)',
              margin: 0,
            }}
          >
            {output || '点击"▶ 运行"执行代码'}
          </pre>
        </div>
      </div>
    </div>
  )
}
