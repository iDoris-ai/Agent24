// iLink QR login. First run prints a QR you scan in WeChat to add the bot;
// the resulting bot_token is persisted so later runs skip the scan. Ported from
// heinu1 (bot/src/ilink/auth.ts).

import fs from 'node:fs'
import path from 'node:path'
import qrcode from 'qrcode-terminal'
import { ILinkPreAuth } from './client.js'
import type { TokenData } from './types.js'
import { CONFIG } from '../config.js'

interface QrCodeResponse {
  qrcode: string // polling key
  qrcode_img_content: string // URL to render as the QR
}

// Reference spells it 'scaned' (single n) — match exactly.
interface QrStatusResponse {
  status: 'wait' | 'scaned' | 'confirmed' | 'expired'
  bot_token?: string
  baseurl?: string
}

export interface LoginResult {
  bot_token: string
  baseurl: string
}

export async function login(): Promise<LoginResult> {
  if (fs.existsSync(CONFIG.TOKEN_FILE)) {
    const data = JSON.parse(fs.readFileSync(CONFIG.TOKEN_FILE, 'utf8')) as TokenData
    console.log('[wechat] 使用已保存的登录 token（如需换号：删除 ' + CONFIG.TOKEN_FILE + '）')
    return { bot_token: data.bot_token, baseurl: data.baseurl }
  }
  return doQRLogin()
}

export async function doQRLogin(): Promise<LoginResult> {
  const pre = new ILinkPreAuth(CONFIG.ILINK_BASE)
  const MAX_TRY = 3

  for (let attempt = 1; attempt <= MAX_TRY; attempt++) {
    console.log(`[wechat] 获取登录二维码 (${attempt}/${MAX_TRY})...`)
    const qr = await pre.get<QrCodeResponse>('/ilink/bot/get_bot_qrcode?bot_type=3')
    if (!qr.qrcode || !qr.qrcode_img_content) {
      throw new Error(`获取二维码失败: ${JSON.stringify(qr).slice(0, 200)}`)
    }

    console.log('\n请用微信扫描以下二维码，把 agent24 加为联系人：\n')
    qrcode.generate(qr.qrcode_img_content, { small: true })
    console.log(`\n二维码 URL: ${qr.qrcode_img_content}\n等待扫码...\n`)

    const deadline = Date.now() + 120_000
    while (Date.now() < deadline) {
      await sleep(2000)
      const st = await pre.get<QrStatusResponse>(
        `/ilink/bot/get_qrcode_status?qrcode=${encodeURIComponent(qr.qrcode)}`,
        { 'iLink-App-ClientVersion': '1' }, // only this endpoint needs it
      )
      if (st.status === 'scaned') {
        process.stdout.write('\r[wechat] 已扫码，等待手机确认...     ')
      } else if (st.status === 'confirmed') {
        if (!st.bot_token) throw new Error('confirmed 但服务器未返回 bot_token')
        const baseurl = st.baseurl || CONFIG.ILINK_BASE
        saveToken(st.bot_token, baseurl)
        console.log('\n[wechat] ✅ 登录成功，token 已保存。')
        return { bot_token: st.bot_token, baseurl }
      } else if (st.status === 'expired') {
        console.log('\n[wechat] 二维码过期，重新获取...')
        break
      }
    }
  }
  throw new Error('多次尝试后仍未完成扫码登录')
}

function saveToken(bot_token: string, baseurl: string): void {
  const data: TokenData = { bot_token, baseurl, saved_at: Date.now() }
  // 0700 on the dir + 0600 on the file: the token authorises acting as your
  // WeChat bot — keep both the file and its parent directory private.
  fs.mkdirSync(path.dirname(CONFIG.TOKEN_FILE), { recursive: true, mode: 0o700 })
  fs.writeFileSync(CONFIG.TOKEN_FILE, JSON.stringify(data, null, 2), { mode: 0o600 })
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
