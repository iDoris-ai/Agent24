// ServiceBox example — long-running Python HTTP service inside a BoxLite VM.
// Demonstrates the M4 pattern: manifest.container → auto-start → /api/svc/<id>/* proxy.
// The service responds to GET /health and GET /api/echo?msg=<text>.
//
// L3: startCmd writes the service script to a temp file inside the VM rather than
// passing it via `python -c '...'` to avoid argument-length limits and injection risk.

import type { CapabilityModule, SimpleRouter } from './base'
import type { CapabilityContext } from '../types'
import { getHostPort } from '../boxlite-service'

// Python service script written to /tmp/svc.py inside the VM at container start.
const PYTHON_SERVICE_SCRIPT = `import json, time
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

class H(BaseHTTPRequestHandler):
    def do_GET(self):
        p = urlparse(self.path)
        if p.path == '/health':
            body = json.dumps({'status':'ok','ts':time.time()}).encode()
        elif p.path == '/api/echo':
            msg = parse_qs(p.query).get('msg', [''])[0]
            body = json.dumps({'echo': msg, 'from': 'service-box'}).encode()
        elif p.path == '/api/info':
            body = json.dumps({'service':'example-service-box','version':'0.1.0'}).encode()
        else:
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', len(body))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass

HTTPServer(('0.0.0.0', 8000), H).serve_forever()
`

// Encode as base64 so the shell command contains no special characters (no escaping needed).
// Inside the VM: decode → write /tmp/svc.py → run with python.
const SCRIPT_B64 = Buffer.from(PYTHON_SERVICE_SCRIPT).toString('base64')
const START_CMD = [
  'sh', '-c',
  `echo ${SCRIPT_B64} | base64 -d > /tmp/svc.py && python /tmp/svc.py`,
]

const serviceBoxModule: CapabilityModule = {
  manifest: {
    id: 'example-service-box',
    version: '0.1.0',
    name: '服务容器示例',
    description: 'BoxLite 长期运行服务容器演示 — Python HTTP 服务，每次请求独立隔离',
    type: 'hybrid',
    permissions: [],
    navItem: {
      icon: '📦',
      label: '服务容器',
      route: 'service-box',
    },
    container: {
      image: 'python:slim',
      port: 8000,
      startCmd: START_CMD,
      healthPath: '/health',
      memoryMib: 256,
    },
  },

  register(router: SimpleRouter, _ctx: CapabilityContext): void {
    // Status endpoint — reports whether the service container is running
    router.get('/api/service-box/status', () => {
      const hostPort = getHostPort('example-service-box')
      return { running: hostPort !== null, hostPort }
    })
    // NOTE: actual service calls go via /api/svc/example-service-box/*
    // which the server proxies directly — no handler needed here.
  },
}

export default serviceBoxModule
