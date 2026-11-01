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
  'presence.title': 'Confirm on this computer',
  'presence.message': '{requester} (pid {pid}) is asking for permission: {operation}',
  'presence.detail': '{summary}\n\nOnly allow this if you started it yourself.',
  'presence.allow': 'Allow',
  'presence.deny': 'Deny',
  'tray.running': '● daemon: running',
  'tray.starting': '○ daemon: starting…',
  'tray.stopped': '✕ daemon: stopped',
  'tray.error': '✕ daemon: incompatible protocol',
  'tray.protocolShort': 'Protocol {daemonVersion} (app {clientVersion}) · pid {pid}',
  'tray.protocolUnknown': 'unknown',
  'tray.protocolRecorded': 'daemon.json recorded protocol {version}.',
  'tray.protocolUnrecorded': 'daemon.json has no protocol version.',
  'tray.protocolDetail': '{summary}. {recorded} {hint}. The external daemon is still running; this app will not stop it or start another daemon.',
  'tray.errorHint': 'Upgrade the older of the app and the daemon, then reopen Agent24',
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
  'comm.export': 'Export the private key of "{name}"?',
  'comm.exportDetail': 'The backup is encrypted with a password you choose next (in a separate Agent24 window), but anyone who gets the file and guesses that password can impersonate you. Store it offline, never in a synced or shared folder. After the password you will choose where to save it.',
  'err.identityNotFound': 'That identity does not exist',
  'err.targetExists': 'A file with that name already exists and will not be overwritten. Choose another file name.',
  'comm.exportSave': 'Save encrypted private key backup',
  'backupPw.title': 'Backup password',
  'backupPw.restoreTitle': 'Restore from backup',
  'backupPw.restoreRules': 'Enter the password this backup was made with.',
  'backupPw.restorePassword': 'Backup password',
  'backupPw.restoreInvalid': 'Enter the backup password (one line, at most 1024 bytes)',
  'backupPw.decrypting': 'Decrypting…',
  'backupPw.badPassword': 'The backup password is incorrect, or the backup is damaged. Try again.',
  'comm.restoreOpen': 'Choose an encrypted backup (.ncryptsec)',
  'err.noBackupTicket': 'The backup file selection expired. Choose the file again.',
  'backupPw.rules': 'Choose a password to encrypt the private key backup: at least 8 characters, at most 1024 bytes, one line, not only spaces.',
  'backupPw.password': 'Backup password', 'backupPw.repeat': 'Repeat backup password',
  'backupPw.warning': 'If you lose this password the backup cannot be restored; Agent24 does not store it.',
  'backupPw.submit': 'Continue', 'backupPw.cancel': 'Cancel',
  'backupPw.mismatch': 'The two passwords do not match',
  'backupPw.invalid': 'The password must be at least 8 characters, at most 1024 bytes, one line and not only spaces',
  'comm.contactAdd': 'Add {name} ({npub}) as a contact?',
  'comm.contactAddDetail': 'Only add an address you received from the person themselves. You can message any contact, and they can message you.',
  'module.change': 'Change domain module "{name}"{from} to {to}?',
  'module.fromEnabled': ' from enabled', 'module.fromDisabled': ' from disabled',
  'module.enabled': 'enabled', 'module.disabled': 'disabled',
  'module.detail': 'Version: {version}\nTakes effect immediately: disabling revokes the module\'s consent and stops it.',
  'err.agentearNotPaired': 'AgentEar is not paired with this Agent24; nothing to change',
  'err.agentearForbidden': 'The daemon refused this operation',
  'err.agentearDaemon': 'The daemon could not complete this operation',
  'agentear.disableMessage': 'Disable AgentEar?',
  'agentear.disableDetail': 'AgentEar disconnects immediately and cannot reconnect until you enable it again here. Its pairing is kept: enabling it later does not need a new pairing.',
  'agentear.enableMessage': 'Enable AgentEar?',
  'agentear.enableDetail': 'AgentEar will be allowed to connect to Agent24 again with its existing pairing.',
  'agentear.unpairMessage': 'Unpair AgentEar?',
  'agentear.unpairDetail': 'AgentEar\'s credential is revoked and it disconnects immediately. This cannot be undone: to use AgentEar with Agent24 again you must pair AgentEar again from its menu bar.',
} as const

export type MainMessageKey = keyof typeof en

const zh: Record<MainMessageKey, string> = {
  'presence.title': '请在本机确认',
  'presence.message': '{requester}（pid {pid}）请求授权：{operation}',
  'presence.detail': '{summary}\n\n只有确定是你自己发起的操作，才选择允许。',
  'presence.allow': '允许',
  'presence.deny': '拒绝',
  'tray.running': '● daemon：运行中',
  'tray.starting': '○ daemon：启动中…',
  'tray.stopped': '✕ daemon：已停止',
  'tray.error': '✕ daemon：协议不兼容',
  'tray.protocolShort': '协议 {daemonVersion}（应用 {clientVersion}）· pid {pid}',
  'tray.protocolUnknown': '未知',
  'tray.protocolRecorded': 'daemon.json 记录的协议版本为 {version}。',
  'tray.protocolUnrecorded': 'daemon.json 没有记录协议版本。',
  'tray.protocolDetail': '{summary}。{recorded}{hint}。外部 daemon 仍在运行，本应用不会终止它，也不会再启动另一个。',
  'tray.errorHint': '请升级应用与 daemon 中较旧的一方，然后重新打开 Agent24',
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
  'comm.export': '导出「{name}」的私钥？',
  'comm.exportDetail': '备份将用你接下来在独立 Agent24 窗口中设置的口令加密，但任何人拿到文件并猜出口令就可以冒充你。请离线保存，不要放在同步盘或共享文件夹。设置口令后再选择保存位置。',
  'err.identityNotFound': '该身份不存在',
  'err.targetExists': '同名文件已存在，不会被覆盖。请换一个文件名。',
  'comm.exportSave': '保存私钥备份',
  'backupPw.title': '备份口令',
  'backupPw.restoreTitle': '从备份恢复',
  'backupPw.restoreRules': '请输入创建此备份时设置的口令。',
  'backupPw.restorePassword': '备份口令',
  'backupPw.restoreInvalid': '请输入备份口令（单行，不超过 1024 字节）',
  'backupPw.decrypting': '正在解密…',
  'backupPw.badPassword': '备份口令不正确，或备份已损坏。请重试。',
  'comm.restoreOpen': '选择加密备份（.ncryptsec）',
  'err.noBackupTicket': '备份文件的选择已过期，请重新选择文件。',
  'backupPw.rules': '请设置加密私钥备份的口令：至少 8 个字符，不超过 1024 字节，单行，不能全是空格。',
  'backupPw.password': '备份口令', 'backupPw.repeat': '再次输入备份口令',
  'backupPw.warning': '如果丢失此口令，备份将无法恢复；Agent24 不会保存它。',
  'backupPw.submit': '继续', 'backupPw.cancel': '取消',
  'backupPw.mismatch': '两次输入的口令不一致',
  'backupPw.invalid': '口令至少 8 个字符、不超过 1024 字节、单行且不能全是空格',
  'comm.contactAdd': '将 {name}（{npub}）添加为联系人？',
  'comm.contactAddDetail': '请只添加你从本人处获得的地址。你可以给任何联系人发消息，对方也可以给你发消息。',
  'module.change': '确认将领域模块「{name}」{from}改为 {to}？',
  'module.fromEnabled': '由 启用 ', 'module.fromDisabled': '由 停用 ',
  'module.enabled': '启用', 'module.disabled': '停用',
  'module.detail': '版本：{version}\n立即生效：停用会立刻撤销该模块的 consent 并停止它。',
  'err.agentearNotPaired': 'AgentEar 尚未与本 Agent24 配对，无需操作',
  'err.agentearForbidden': '守护进程拒绝了此操作',
  'err.agentearDaemon': '守护进程未能完成此操作',
  'agentear.disableMessage': '确认停用 AgentEar？',
  'agentear.disableDetail': 'AgentEar 会立即断开，在你回到这里重新启用之前无法再连接。配对关系保留：之后启用不需要重新配对。',
  'agentear.enableMessage': '确认启用 AgentEar？',
  'agentear.enableDetail': 'AgentEar 将被允许用现有配对再次连接 Agent24。',
  'agentear.unpairMessage': '确认解除与 AgentEar 的配对？',
  'agentear.unpairDetail': 'AgentEar 的凭据会被撤销并立即断开。此操作不可撤回：之后要在 Agent24 中继续使用 AgentEar，必须在 AgentEar 菜单栏里重新配对。',
}

const th: Record<MainMessageKey, string> = {
  'presence.title': 'ยืนยันบนเครื่องนี้',
  'presence.message': '{requester} (pid {pid}) ขออนุญาต: {operation}',
  'presence.detail': '{summary}\n\nอนุญาตเฉพาะเมื่อคุณเป็นผู้เริ่มการดำเนินการนี้เอง',
  'presence.allow': 'อนุญาต',
  'presence.deny': 'ปฏิเสธ',
  'tray.running': '● daemon: กำลังทำงาน',
  'tray.starting': '○ daemon: กำลังเริ่ม…',
  'tray.stopped': '✕ daemon: หยุดแล้ว',
  'tray.error': '✕ daemon: โปรโตคอลไม่ตรงกัน',
  'tray.protocolShort': 'โปรโตคอล {daemonVersion} (แอป {clientVersion}) · pid {pid}',
  'tray.protocolUnknown': 'ไม่ทราบ',
  'tray.protocolRecorded': 'daemon.json มีเวอร์ชันโปรโตคอลที่บันทึกไว้ {version}',
  'tray.protocolUnrecorded': 'daemon.json ไม่ได้บันทึกเวอร์ชันโปรโตคอล',
  'tray.protocolDetail': '{summary}. {recorded} {hint} daemon ภายนอกยังทำงานอยู่ แอปนี้จะไม่หยุด daemon นั้นหรือเริ่ม daemon อีกตัว',
  'tray.errorHint': 'อัปเดตแอปหรือ daemon ฝั่งที่เก่ากว่า แล้วเปิด Agent24 ใหม่',
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
  'comm.export': 'ส่งออกคีย์ส่วนตัวของ "{name}"?',
  'comm.exportDetail': 'ไฟล์สำรองจะเข้ารหัสด้วยรหัสผ่านที่คุณตั้งต่อไปในหน้าต่าง Agent24 แยกต่างหาก แต่ใครที่ได้ไฟล์และเดารหัสผ่านถูกจะแอบอ้างเป็นคุณได้ โปรดเก็บแบบออฟไลน์ ไม่ใช่ในโฟลเดอร์ที่ซิงก์หรือแชร์ หลังตั้งรหัสผ่านแล้วจึงเลือกที่บันทึก',
  'err.identityNotFound': 'ไม่มีตัวตนนี้',
  'err.targetExists': 'มีไฟล์ชื่อนี้อยู่แล้วและจะไม่ถูกเขียนทับ โปรดเลือกชื่อไฟล์อื่น',
  'comm.exportSave': 'บันทึกไฟล์สำรองคีย์ส่วนตัว',
  'backupPw.title': 'รหัสผ่านสำรอง',
  'backupPw.restoreTitle': 'กู้คืนจากไฟล์สำรอง',
  'backupPw.restoreRules': 'กรอกรหัสผ่านที่ใช้ตอนสร้างไฟล์สำรองนี้',
  'backupPw.restorePassword': 'รหัสผ่านสำรอง',
  'backupPw.restoreInvalid': 'กรอกรหัสผ่านสำรอง (บรรทัดเดียว ไม่เกิน 1024 ไบต์)',
  'backupPw.decrypting': 'กำลังถอดรหัส…',
  'backupPw.badPassword': 'รหัสผ่านสำรองไม่ถูกต้อง หรือไฟล์สำรองเสียหาย โปรดลองอีกครั้ง',
  'comm.restoreOpen': 'เลือกไฟล์สำรองที่เข้ารหัส (.ncryptsec)',
  'err.noBackupTicket': 'การเลือกไฟล์สำรองหมดอายุแล้ว โปรดเลือกไฟล์อีกครั้ง',
  'backupPw.rules': 'ตั้งรหัสผ่านเพื่อเข้ารหัสไฟล์สำรองคีย์ส่วนตัว: อย่างน้อย 8 ตัวอักษร ไม่เกิน 1024 ไบต์ บรรทัดเดียว และไม่ใช่ช่องว่างล้วน',
  'backupPw.password': 'รหัสผ่านสำรอง', 'backupPw.repeat': 'ยืนยันรหัสผ่านสำรอง',
  'backupPw.warning': 'หากทำรหัสผ่านนี้หาย จะกู้คืนไฟล์สำรองไม่ได้ และ Agent24 ไม่เก็บรหัสผ่านนี้ไว้',
  'backupPw.submit': 'ดำเนินการต่อ', 'backupPw.cancel': 'ยกเลิก',
  'backupPw.mismatch': 'รหัสผ่านสองครั้งไม่ตรงกัน',
  'backupPw.invalid': 'รหัสผ่านต้องมีอย่างน้อย 8 ตัวอักษร ไม่เกิน 1024 ไบต์ บรรทัดเดียว และไม่ใช่ช่องว่างล้วน',
  'comm.contactAdd': 'เพิ่ม {name} ({npub}) เป็นผู้ติดต่อ?',
  'comm.contactAddDetail': 'เพิ่มเฉพาะที่อยู่ที่ได้รับจากเจ้าของโดยตรง คุณส่งข้อความหาผู้ติดต่อใดก็ได้ และเขาก็ส่งหาคุณได้',
  'module.change': 'ยืนยันเปลี่ยนโมดูลโดเมน "{name}"{from}เป็น {to}?',
  'module.fromEnabled': 'จาก เปิดใช้งาน ', 'module.fromDisabled': 'จาก ปิดใช้งาน ',
  'module.enabled': 'เปิดใช้งาน', 'module.disabled': 'ปิดใช้งาน',
  'module.detail': 'เวอร์ชัน: {version}\nมีผลทันที: การปิดใช้งานจะเพิกถอน consent ของโมดูลและหยุดโมดูลนั้นทันที',
  'err.agentearNotPaired': 'AgentEar ยังไม่ได้จับคู่กับ Agent24 นี้ ไม่มีอะไรต้องเปลี่ยน',
  'err.agentearForbidden': 'daemon ปฏิเสธการดำเนินการนี้',
  'err.agentearDaemon': 'daemon ดำเนินการนี้ไม่สำเร็จ',
  'agentear.disableMessage': 'ยืนยันปิดใช้งาน AgentEar?',
  'agentear.disableDetail': 'AgentEar จะตัดการเชื่อมต่อทันทีและเชื่อมต่อใหม่ไม่ได้จนกว่าคุณจะเปิดใช้งานอีกครั้งที่นี่ การจับคู่ยังคงอยู่ เปิดใช้งานภายหลังได้โดยไม่ต้องจับคู่ใหม่',
  'agentear.enableMessage': 'ยืนยันเปิดใช้งาน AgentEar?',
  'agentear.enableDetail': 'AgentEar จะได้รับอนุญาตให้เชื่อมต่อกับ Agent24 อีกครั้งด้วยการจับคู่เดิม',
  'agentear.unpairMessage': 'ยืนยันยกเลิกการจับคู่ AgentEar?',
  'agentear.unpairDetail': 'ข้อมูลรับรองของ AgentEar จะถูกเพิกถอนและตัดการเชื่อมต่อทันที ย้อนกลับไม่ได้ หากต้องการใช้ AgentEar กับ Agent24 อีก ต้องจับคู่ใหม่จากแถบเมนูของ AgentEar',
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
