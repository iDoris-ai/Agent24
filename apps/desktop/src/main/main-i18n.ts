// Main-process UI language (tray menu, native dialogs, user-facing IPC errors).
//
// The language the user picks lives in the renderer (`agent24:locale` in
// localStorage, see renderer/i18n), which main cannot read. So the renderer
// reports it over IPC (`app:set-locale`) at startup and on every change, and
// main keeps its own copy in `<userData>/ui-locale.json` so the tray is right
// before the first window has loaded.
//
// Default is English. The system locale is deliberately NOT consulted.

import fs from 'node:fs'
import path from 'node:path'
import { cliToolMessages } from '../shared/cli-tool.i18n'

export type MainLocale = 'en' | 'zh' | 'th'
export const MAIN_DEFAULT_LOCALE: MainLocale = 'en'
const FILE_NAME = 'ui-locale.json'

export function isMainLocale(v: unknown): v is MainLocale {
  return v === 'en' || v === 'zh' || v === 'th'
}

const en = {
  'tray.running': '● daemon: running',
  'tray.starting': '○ daemon: starting…',
  'tray.stopped': '✕ daemon: stopped',
  'tray.blocked': '✕ daemon: in use by another Agent24',
  'tray.external': '{label} (external daemon · pid {pid})',
  'tray.foreign': '{label} (pid {pid})',
  'tray.start': 'Start daemon',
  'tray.disconnect': 'Disconnect external daemon (does not stop it)',
  'tray.restart': 'Restart daemon',
  'tray.stop': 'Stop daemon',
  'tray.show': 'Show window',
  'tray.quit': 'Quit Agent24',
  'cli.missing': cliToolMessages.en.missing,
  'cli.outdated': cliToolMessages.en.outdated,
  'cli.unknown': cliToolMessages.en.unknown,
  'cli.confirmTitle': 'Replace the existing agent24 command?',
  'cli.confirmDetail': 'The existing /usr/local/bin/agent24 will be renamed to agent24.bak-<date> in the same directory. A symbolic link to this App’s CLI will take its place.',
  'cli.replace': 'Back up and replace',
  'dialog.confirm': 'Confirm',
  'dialog.cancel': 'Cancel',
  'clip.truncated': '… (truncated, {length} characters in total)',
  'err.busy': 'Another operation awaiting confirmation is in progress',
  'err.cancelled': 'Operation cancelled',
  'err.approvalUnavailable': 'Could not read the real record of this approval; operation refused',
  'err.notPending': 'This approval is already {status}',
  'err.moduleUnavailable': 'The daemon has no such domain module; operation refused',
  'approval.approve': 'approve', 'approval.approve_for_session': 'approve for this session',
  'approval.deny': 'deny', 'approval.abort': 'abort',
  'approval.message': 'Confirm the decision "{decision}" for approval {id}?',
  'approval.kind': 'Kind: {value}', 'approval.summary': 'Summary: {value}', 'approval.run': 'Run: {value}',
  'approval.details': 'Details: {value}', 'approval.note': 'Note from the page (for reference only): {value}',
  'approval.warning': 'This decision takes effect immediately and may not be reversible.',
  'comm.install': 'Download and set up Hyphae?',
  'comm.installDetail': 'Agent24 will download the pinned Hyphae executable from GitHub and verify its SHA-256 before installation. Communication will become available without restarting Agent24.',
  'module.change': 'Change domain module "{name}"{from} to {to}?',
  'module.fromEnabled': ' from enabled', 'module.fromDisabled': ' from disabled',
  'module.enabled': 'enabled', 'module.disabled': 'disabled',
  'module.detail': 'Version: {version}\nTakes effect immediately: disabling revokes the module\'s consent and stops it.',
} as const

export type MainMessageKey = keyof typeof en

const zh: Record<MainMessageKey, string> = {
  'tray.running': '● daemon：运行中',
  'tray.starting': '○ daemon：启动中…',
  'tray.stopped': '✕ daemon：已停止',
  'tray.blocked': '✕ daemon：被另一个 Agent24 占用',
  'tray.external': '{label}（外部 daemon · pid {pid}）',
  'tray.foreign': '{label}（pid {pid}）',
  'tray.start': '启动 daemon',
  'tray.disconnect': '断开外部 daemon（不会终止它）',
  'tray.restart': '重启 daemon',
  'tray.stop': '停止 daemon',
  'tray.show': '显示窗口',
  'tray.quit': '退出 Agent24',
  'cli.missing': cliToolMessages.zh.missing,
  'cli.outdated': cliToolMessages.zh.outdated,
  'cli.unknown': cliToolMessages.zh.unknown,
  'cli.confirmTitle': '替换已有的 agent24 命令？',
  'cli.confirmDetail': '已有的 /usr/local/bin/agent24 将在原目录改名为 agent24.bak-<日期>，再由指向本应用 CLI 的符号链接替代。',
  'cli.replace': '备份并替换',
  'dialog.confirm': '确认',
  'dialog.cancel': '取消',
  'clip.truncated': '…（已截断，共 {length} 字符）',
  'err.busy': '另一项需要确认的操作正在进行',
  'err.cancelled': '操作已取消',
  'err.approvalUnavailable': '读不到该审批的真实记录，已拒绝操作',
  'err.notPending': '审批已处于 {status} 状态',
  'err.moduleUnavailable': '守护进程没有该领域模块，已拒绝操作',
  'approval.approve': '批准', 'approval.approve_for_session': '本会话内一律批准',
  'approval.deny': '拒绝', 'approval.abort': '中止',
  'approval.message': '确认对审批 {id} 做出「{decision}」决定？',
  'approval.kind': '类型：{value}', 'approval.summary': '摘要：{value}', 'approval.run': '运行：{value}',
  'approval.details': '详情：{value}', 'approval.note': '页面备注（仅供参考）：{value}',
  'approval.warning': '此决定会立即生效，且可能无法撤销。',
  'comm.install': '确认下载并设置 Hyphae？',
  'comm.installDetail': 'Agent24 将从 GitHub 下载固定版本的 Hyphae 程序，校验 SHA-256 后安装。无需重启 Agent24 即可使用通信功能。',
  'module.change': '确认将领域模块「{name}」{from}改为 {to}？',
  'module.fromEnabled': '由 启用 ', 'module.fromDisabled': '由 停用 ',
  'module.enabled': '启用', 'module.disabled': '停用',
  'module.detail': '版本：{version}\n立即生效：停用会立刻撤销该模块的 consent 并停止它。',
}

const th: Record<MainMessageKey, string> = {
  'tray.running': '● daemon: กำลังทำงาน',
  'tray.starting': '○ daemon: กำลังเริ่ม…',
  'tray.stopped': '✕ daemon: หยุดแล้ว',
  'tray.blocked': '✕ daemon: ถูก Agent24 อื่นใช้งานอยู่',
  'tray.external': '{label} (daemon ภายนอก · pid {pid})',
  'tray.foreign': '{label} (pid {pid})',
  'tray.start': 'เริ่ม daemon',
  'tray.disconnect': 'ตัดการเชื่อมต่อ daemon ภายนอก (ไม่หยุดการทำงานของมัน)',
  'tray.restart': 'รีสตาร์ต daemon',
  'tray.stop': 'หยุด daemon',
  'tray.show': 'แสดงหน้าต่าง',
  'tray.quit': 'ออกจาก Agent24',
  'cli.missing': cliToolMessages.th.missing,
  'cli.outdated': cliToolMessages.th.outdated,
  'cli.unknown': cliToolMessages.th.unknown,
  'cli.confirmTitle': 'แทนที่คำสั่ง agent24 ที่มีอยู่หรือไม่?',
  'cli.confirmDetail': 'จะเปลี่ยนชื่อ /usr/local/bin/agent24 เดิมเป็น agent24.bak-<วันที่> ในโฟลเดอร์เดิม แล้วสร้างลิงก์สัญลักษณ์ไปยัง CLI ของแอปนี้',
  'cli.replace': 'สำรองและแทนที่',
  'dialog.confirm': 'ยืนยัน',
  'dialog.cancel': 'ยกเลิก',
  'clip.truncated': '… (ตัดทอนแล้ว รวม {length} ตัวอักษร)',
  'err.busy': 'มีการดำเนินการอื่นที่รอการยืนยันอยู่',
  'err.cancelled': 'ยกเลิกการดำเนินการแล้ว',
  'err.approvalUnavailable': 'อ่านบันทึกจริงของการอนุมัตินี้ไม่ได้ ปฏิเสธการดำเนินการ',
  'err.notPending': 'การอนุมัตินี้อยู่ในสถานะ {status} แล้ว',
  'err.moduleUnavailable': 'daemon ไม่มีโมดูลโดเมนนี้ ปฏิเสธการดำเนินการ',
  'approval.approve': 'อนุมัติ', 'approval.approve_for_session': 'อนุมัติตลอดเซสชันนี้',
  'approval.deny': 'ปฏิเสธ', 'approval.abort': 'ยกเลิกการทำงาน',
  'approval.message': 'ยืนยันการตัดสินใจ "{decision}" สำหรับการอนุมัติ {id}?',
  'approval.kind': 'ประเภท: {value}', 'approval.summary': 'สรุป: {value}', 'approval.run': 'การรัน: {value}',
  'approval.details': 'รายละเอียด: {value}', 'approval.note': 'หมายเหตุจากหน้าเว็บ (เพื่อประกอบเท่านั้น): {value}',
  'approval.warning': 'การตัดสินใจนี้มีผลทันที และอาจย้อนกลับไม่ได้',
  'comm.install': 'ดาวน์โหลดและตั้งค่า Hyphae?',
  'comm.installDetail': 'Agent24 จะดาวน์โหลดโปรแกรม Hyphae รุ่นที่กำหนดจาก GitHub ตรวจสอบ SHA-256 แล้วติดตั้ง โดยไม่ต้องเริ่ม Agent24 ใหม่',
  'module.change': 'ยืนยันเปลี่ยนโมดูลโดเมน "{name}"{from}เป็น {to}?',
  'module.fromEnabled': 'จาก เปิดใช้งาน ', 'module.fromDisabled': 'จาก ปิดใช้งาน ',
  'module.enabled': 'เปิดใช้งาน', 'module.disabled': 'ปิดใช้งาน',
  'module.detail': 'เวอร์ชัน: {version}\nมีผลทันที: การปิดใช้งานจะเพิกถอน consent ของโมดูลและหยุดโมดูลนั้นทันที',
}

const TABLE: Record<MainLocale, Record<MainMessageKey, string>> = { en, zh, th }

let current: MainLocale = MAIN_DEFAULT_LOCALE
let storeDir: string | null = null
const listeners = new Set<(locale: MainLocale) => void>()

export function getMainLocale(): MainLocale {
  return current
}

/** Translate for the current language. */
export function mt(key: MainMessageKey, params: Record<string, string | number> = {}): string {
  return TABLE[current][key].replace(/\{(\w+)\}/g, (m, name: string) => (name in params ? String(params[name]) : m))
}

export function onMainLocaleChange(fn: (locale: MainLocale) => void): () => void {
  listeners.add(fn)
  return () => { listeners.delete(fn) }
}

/** Switch language (and remember it). Unknown values are ignored. */
export function setMainLocale(locale: unknown): void {
  if (!isMainLocale(locale)) return
  if (storeDir) {
    try { fs.writeFileSync(path.join(storeDir, FILE_NAME), JSON.stringify({ locale }), { mode: 0o600 }) } catch { /* not persisted; still switch */ }
  }
  if (locale === current) return
  current = locale
  listeners.forEach((fn) => fn(locale))
}

/** Read the remembered language from `dir`; English when there is none. */
export function initMainLocale(dir: string): void {
  storeDir = dir
  current = MAIN_DEFAULT_LOCALE
  try {
    const parsed = JSON.parse(fs.readFileSync(path.join(dir, FILE_NAME), 'utf8')) as { locale?: unknown }
    if (isMainLocale(parsed.locale)) current = parsed.locale
  } catch { /* none yet, or unreadable: English */ }
}

/** Test seam: back to the English default with nothing remembered. */
export function resetMainLocaleForTest(): void {
  current = MAIN_DEFAULT_LOCALE
  storeDir = null
  listeners.clear()
}
