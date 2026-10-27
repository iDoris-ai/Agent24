import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  OpenDesignComponentInstaller,
  openDesignComponentInstallDir,
  type OpenDesignComponentManifest,
} from './open-design-component'

// This file does real tar/spawn/HTTP round trips (building fixture
// tarballs, running `tar`, serving them over a local server) rather than
// pure in-process unit work — comfortably under 2s each in isolation, but
// the default 5s test timeout is tight under concurrent machine load (CI
// and shared dev boxes alike). 15s leaves plenty of margin while still
// catching a genuine hang (see the explicit M5/H2 abort tests, which prove
// a real hang is caught well before this).
vi.setConfig({ testTimeout: 30_000 })

const tempDirs: string[] = []

// M4 (Opus re-review, 2026-10-05): a successful install now chmod's the
// whole installed tree read-only (0500 dirs / 0444 files) — a plain
// fs.rmSync can no longer remove it (ENOTEMPTY: deleting a directory entry
// needs write on its PARENT, which 0500 deliberately removed). Restore
// write access bottom-up before removing, mirroring what the installer
// itself does before a re-install (makeInstalledTreeWritableForRemoval in
// the source file).
function restoreWritableRecursive(root: string): void {
  const stat = fs.lstatSync(root, { throwIfNoEntry: false })
  if (stat == null || stat.isSymbolicLink()) return
  if (stat.isDirectory()) {
    fs.chmodSync(root, 0o700)
    for (const entry of fs.readdirSync(root)) restoreWritableRecursive(path.join(root, entry))
  } else if (stat.isFile()) {
    fs.chmodSync(root, 0o600)
  }
}

function rmTreeEvenIfReadOnly(root: string): void {
  restoreWritableRecursive(root)
  fs.rmSync(root, { recursive: true, force: true })
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) rmTreeEvenIfReadOnly(dir)
  vi.restoreAllMocks()
})

function tmpDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
  tempDirs.push(dir)
  return dir
}

function seedValidTree(root: string): void {
  for (const relative of ['app/prebundled/daemon', 'open-design', 'open-design-web-standalone']) {
    fs.mkdirSync(path.join(root, relative), { recursive: true })
  }
  fs.writeFileSync(path.join(root, 'app/package.json'), JSON.stringify({ version: '0.22.2' }))
  for (const relative of [
    'app/prebundled/agent24-headless.cjs',
    'app/prebundled/agent24-headless.mjs',
    'app/prebundled/daemon/daemon-cli.mjs',
    'app/prebundled/daemon/daemon-sidecar.mjs',
    'app/prebundled/web-sidecar.mjs',
  ]) fs.writeFileSync(path.join(root, relative), '')
  fs.writeFileSync(path.join(root, 'open-design-web-standalone/server.js'), '')
}

function buildTarball(stageDir: string): string {
  const tarPath = path.join(path.dirname(stageDir), `${path.basename(stageDir)}.tar.gz`)
  execFileSync('tar', ['-czf', tarPath, '-C', stageDir, 'app', 'open-design', 'open-design-web-standalone'])
  tempDirs.push(tarPath)
  return tarPath
}

function seedTreeFor(prefix: string): string {
  const root = tmpDir(prefix)
  seedValidTree(root)
  return root
}

// CI finding (2026-10-05): this used to hardcode platform:'darwin'/
// arch:'arm64'. OpenDesignComponentInstaller's L2 check compares the
// manifest's platform/arch against the REAL process.platform/process.arch
// whenever a test doesn't inject an explicit override (most of this file
// doesn't — only the dedicated L2 test below does) — so on a Linux CI
// runner, every one of those installers rejected the hardcoded-darwin
// fixture as a platform mismatch and reported `unavailable` instead of
// `not-installed`/`installed`. Defaulting to the actual host's
// platform/arch here makes every other test in this file host-independent.
function manifestFor(tarPath: string, overrides: Partial<OpenDesignComponentManifest> = {}): { manifest: OpenDesignComponentManifest; sha256: string; size: number } {
  const bytes = fs.readFileSync(tarPath)
  const sha256 = crypto.createHash('sha256').update(bytes).digest('hex')
  const manifest: OpenDesignComponentManifest = {
    version: '0.22.2',
    odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f',
    platform: process.platform,
    arch: process.arch,
    url: 'https://example.invalid/placeholder.tar.gz',
    sha256,
    size: bytes.length,
    ...overrides,
  }
  return { manifest, sha256, size: bytes.length }
}


function reviewSetup() {
  const stage = seedTreeFor('a24-review-stage-')
  const tarPath = buildTarball(stage)
  const { manifest } = manifestFor(tarPath)
  const bytes = fs.readFileSync(tarPath)
  const root = tmpDir('a24-review-root-')
  const part = path.join(root, `.download-${manifest.sha256}.part`)
  const meta = path.join(root, `.download-${manifest.sha256}.meta.json`)
  const metadata = JSON.stringify({version: manifest.version, url: manifest.url, sha256: manifest.sha256, total: manifest.size})
  return {manifest, bytes, root, part, meta, metadata}
}
function reviewInstaller(f: ReturnType<typeof reviewSetup>, fetcher: typeof fetch) {
  return new OpenDesignComponentInstaller({manifest:f.manifest, componentsRoot:f.root, allowedHosts:['example.invalid']}, fetcher)
}
function reviewResume(f: ReturnType<typeof reviewSetup>, prefix = 3) {
  fs.writeFileSync(f.part,f.bytes.subarray(0,prefix))
  fs.writeFileSync(f.meta,f.metadata)
}
function review206(f: ReturnType<typeof reviewSetup>, end = f.bytes.length - 1): Response {
 return new Response(f.bytes.subarray(3), {status:206, headers:{'Content-Range':`bytes 3-${end}/${f.bytes.length}`}})
}
describe('Independent exact-head review',()=>{
  it('P1 part symlink must never append outside componentsRoot',async()=>{
    const f=reviewSetup(); reviewResume(f)
    const external=path.join(tmpDir('a24-review-external-'),'victim')
    const before=f.bytes.subarray(0,3); fs.writeFileSync(external,before)
    fs.rmSync(f.part); fs.symlinkSync(external,f.part)
    await reviewInstaller(f,async()=>review206(f)).install()
    expect(fs.readFileSync(external)).toEqual(before)
  })
  it('P1 metadata symlink must never rewrite outside componentsRoot',async()=>{
    const f=reviewSetup(); reviewResume(f)
    const external=path.join(tmpDir('a24-review-external-'),'victim')
    const before=JSON.stringify(JSON.parse(f.metadata),null,2); fs.writeFileSync(external,before)
    fs.rmSync(f.meta); fs.symlinkSync(external,f.meta)
    await reviewInstaller(f,async()=>review206(f)).install()
    expect(fs.readFileSync(external,'utf8')).toBe(before)
  })
  it('P1 abort during verifying must prevent publish and leave old cache',async()=>{
    const f=reviewSetup(); const old=path.join(f.root,'older-installed');fs.mkdirSync(old);fs.writeFileSync(path.join(old,'keep'),'keep')
    const i=reviewInstaller(f,async()=>new Response(f.bytes))
    let observed!:()=>void;const verifying=new Promise<void>(r=>{observed=r})
    i.onProgress(s=>{if(s.state==='verifying')observed()})
    const pending=i.install();await verifying;i.abort('user cancelled verification')
    const result=await pending
    expect(fs.readFileSync(path.join(old,'keep'),'utf8')).toBe('keep')
    expect(result.state).not.toBe('installed')
    expect(fs.existsSync(openDesignComponentInstallDir(f.root,f.manifest))).toBe(false)
  })
  it('P1 pause while complete prefix is hashing must prevent publish',async()=>{
    const f=reviewSetup();fs.writeFileSync(f.part,f.bytes);fs.writeFileSync(f.meta,f.metadata)
    const i=reviewInstaller(f,async()=>{throw Error('network forbidden')})
    const pending=i.install();i.pause()
    const result=await pending
    expect(result.state).toBe('paused')
    expect(fs.existsSync(openDesignComponentInstallDir(f.root,f.manifest))).toBe(false)
  })
  it('206 invalid end must be rejected even when bytes hash correctly',async()=>{
    const f=reviewSetup(); reviewResume(f);let requests=0
    const i=reviewInstaller(f,async()=>{requests++;return requests===1?review206(f,1):new Response(f.bytes)})
    expect((await i.install()).state).toBe('installed')
    expect(requests).toBe(2)
  })
  it('synchronous install reentry must share one network job',async()=>{
    const f=reviewSetup();let requests=0;let nested:Promise<unknown>|undefined
    const i=reviewInstaller(f,async()=>{requests++;return new Response(f.bytes)})
    i.onProgress(s=>{if(s.state==='downloading'&&!nested){nested=Promise.resolve();nested=i.install()}})
    const first=i.install();await Promise.all([first,nested])
    expect(requests).toBe(1)
  })
  it('idle transfer must stop displaying a stale positive speed',async()=>{
    const f=reviewSetup(); let now=100000;vi.spyOn(Date,'now').mockImplementation(()=>now)
    let stream!:ReadableStreamDefaultController<Uint8Array>
    const i=reviewInstaller(f,async(_input,init)=>new Response(new ReadableStream({start(c){stream=c;init?.signal?.addEventListener('abort',()=>c.error(init.signal?.reason))}})))
    const p=i.install();await vi.waitFor(()=>expect(stream).toBeDefined())
    stream.enqueue(f.bytes.subarray(0,1));await vi.waitFor(()=>expect(i.status().received).toBe(1))
    now+=200;stream.enqueue(f.bytes.subarray(1,2));await vi.waitFor(()=>expect(i.status().received).toBe(2))
    expect(i.status().bytesPerSecond).toBeGreaterThan(0)
    now+=4000
    const idle=i.status();i.pause();await p
    expect(idle.bytesPerSecond).toBe(0)
    expect(idle.etaSeconds).toBeNull()
  })
  it('corrupt complete prefix must fail full SHA and never fetch or publish',async()=>{
    const f=reviewSetup();const corrupt=Buffer.from(f.bytes);corrupt[0]^=1
    fs.writeFileSync(f.part,corrupt);fs.writeFileSync(f.meta,f.metadata)
    const fetcher=vi.fn(async()=>new Response(f.bytes));const result=await reviewInstaller(f,fetcher).install()
    expect(result.state).toBe('failed');expect(result.error).toMatch(/sha256/);expect(fetcher).not.toHaveBeenCalled()
    expect(fs.existsSync(f.part)).toBe(false);expect(fs.existsSync(f.meta)).toBe(false)
    expect(fs.existsSync(openDesignComponentInstallDir(f.root,f.manifest))).toBe(false)
  })
  it('corrupt short prefix must fail final full SHA and never publish',async()=>{
    const f=reviewSetup();reviewResume(f);fs.writeFileSync(f.part,Buffer.alloc(3,7))
    const result=await reviewInstaller(f,async()=>review206(f)).install()
    expect(result.state).toBe('failed');expect(result.error).toMatch(/sha256/)
    expect(fs.existsSync(f.part)).toBe(false);expect(fs.existsSync(f.meta)).toBe(false)
    expect(fs.existsSync(openDesignComponentInstallDir(f.root,f.manifest))).toBe(false)
  })
  it('valid complete prefix must verify and install without network',async()=>{
    const f=reviewSetup();fs.writeFileSync(f.part,f.bytes);fs.writeFileSync(f.meta,f.metadata)
    const fetcher=vi.fn(async()=>new Response(f.bytes));expect((await reviewInstaller(f,fetcher).install()).state).toBe('installed')
    expect(fetcher).not.toHaveBeenCalled()
  })
  it('overflow beyond manifest must clear current artifacts',async()=>{
    const f=reviewSetup();reviewResume(f)
    expect((await reviewInstaller(f,async()=>new Response(Buffer.concat([f.bytes,Buffer.from('overflow')]))).install()).state).toBe('failed')
    expect(fs.existsSync(f.part)).toBe(false);expect(fs.existsSync(f.meta)).toBe(false)
  })
  it('repeated 416 is bounded at two requests',async()=>{
    const f=reviewSetup();reviewResume(f);const fetcher=vi.fn(async()=>new Response(null,{status:416}))
    expect((await reviewInstaller(f,fetcher).install()).state).toBe('failed');expect(fetcher).toHaveBeenCalledTimes(2)
  })
  it.each(['url','version','sha256','total'])('different metadata %s never resumes',async(key)=>{
    const f=reviewSetup();reviewResume(f);const m=JSON.parse(f.metadata);m[key]=key==='total'?f.bytes.length+1:'wrong';fs.writeFileSync(f.meta,JSON.stringify(m))
    let headers:HeadersInit|undefined
    const i=reviewInstaller(f,async(_input,init)=>{headers=init?.headers;return new Response(f.bytes)})
    expect(i.status().state).toBe('not-installed');expect((await i.install()).state).toBe('installed');expect(new Headers(headers).has('range')).toBe(false)
  })
  it('new instance observes paused prefix without automatic fetch',()=>{
    const f=reviewSetup();reviewResume(f);const fetcher=vi.fn(async()=>new Response(f.bytes))
    expect(reviewInstaller(f,fetcher).status()).toMatchObject({state:'paused',received:3,total:f.bytes.length});expect(fetcher).not.toHaveBeenCalled()
  })
  it('200 ignores range and overwrites instead of append',async()=>{
    const f=reviewSetup();reviewResume(f);expect((await reviewInstaller(f,async()=>new Response(f.bytes)).install()).state).toBe('installed')
  })
  it('truncated body preserves actual prefix for explicit later continue',async()=>{
    const f=reviewSetup();const result=await reviewInstaller(f,async()=>new Response(f.bytes.subarray(0,7))).install()
    expect(result.state).toBe('failed');expect(fs.readFileSync(f.part)).toEqual(f.bytes.subarray(0,7))
    const i=reviewInstaller(f,async()=>new Response(f.bytes.subarray(7),{status:206,headers:{'content-range':`bytes 7-${f.bytes.length-1}/${f.bytes.length}`}}))
    expect(i.status().state).toBe('paused');expect((await i.install()).state).toBe('installed')
  })
})

describe('additional rejection and transfer fixtures',()=>{
  it('rejected archive cleans this attempt and preserves installed old version',async()=>{
    const f=reviewSetup();const old=openDesignComponentInstallDir(f.root,f.manifest);seedValidTree(old)
    const oldMarker=JSON.stringify({sha256:'a'.repeat(64)});fs.writeFileSync(path.join(old,'.agent24-component.json'),oldMarker)
    const wrong=Buffer.alloc(f.bytes.length);f.manifest.sha256=crypto.createHash('sha256').update(wrong).digest('hex')
    // Download SHA is valid but archive is invalid; extraction/listing must reject.
    const result=await reviewInstaller(f,async()=>new Response(wrong)).install()
    expect(result.state).toBe('failed');expect(fs.readFileSync(path.join(old,'.agent24-component.json'),'utf8')).toBe(oldMarker)
    expect(fs.readdirSync(f.root).filter(x=>x.startsWith('.download-')||x.startsWith('.partial-'))).toEqual([])
  })
  it('pause flushes actual bytes and a different instance explicitly resumes',async()=>{
    const f=reviewSetup();let stream!:ReadableStreamDefaultController<Uint8Array>
    const i=reviewInstaller(f,async(_input,init)=>new Response(new ReadableStream({start(c){stream=c;init?.signal?.addEventListener('abort',()=>c.error(init.signal?.reason))}})))
    const p=i.install();await vi.waitFor(()=>expect(stream).toBeDefined());stream.enqueue(f.bytes.subarray(0,7))
    await vi.waitFor(()=>expect(i.status().received).toBe(7));i.pause();expect((await p).state).toBe('paused')
    expect(fs.readFileSync(f.part)).toEqual(f.bytes.subarray(0,7))
    let range:string|null=null
    const newer=reviewInstaller(f,async(_input,init)=>{range=new Headers(init?.headers).get('range');return new Response(f.bytes.subarray(7),{status:206,headers:{'content-range':`bytes 7-${f.bytes.length-1}/${f.bytes.length}`}})})
    expect(newer.status().state).toBe('paused');expect((await newer.install()).state).toBe('installed');expect(range).toBe('bytes=7-')
  })
})
