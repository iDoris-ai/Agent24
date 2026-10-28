import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync, spawn } from 'node:child_process'
import { expect, it } from 'vitest'
import { readOmlxSettingsKey } from './omlx-settings-key'

it.skipIf(process.platform === 'win32')('settings.json 是 FIFO 时立即忽略，不阻塞等待写入端', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-omlx-fifo-'))
  const file = path.join(dir, 'settings.json')
  execFileSync('mkfifo', [file])
  // 旧实现会等待此写入端；延迟打开让反面对照能失败退出，避免挂住测试进程。
  const writer = spawn(process.execPath, ['-e', `
    const fs = require('node:fs')
    setTimeout(() => {
      const fd = fs.openSync(process.argv[1], fs.constants.O_RDWR | fs.constants.O_NONBLOCK)
      setTimeout(() => fs.closeSync(fd), 100)
    }, 1500)
  `, file])
  const exited = new Promise<void>((resolve, reject) => {
    writer.once('error', reject)
    writer.once('exit', (code) => code === 0 ? resolve() : reject(new Error('FIFO 写入端失败')))
  })
  try {
    const start = performance.now()
    expect(readOmlxSettingsKey(file)).toBeNull()
    expect(performance.now() - start).toBeLessThan(500)
  } finally {
    await exited
    fs.rmSync(dir, { recursive: true, force: true })
  }
})
