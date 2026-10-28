import type { components } from '@agent24/api-client'
import { defineMessages, type Locale } from '../../i18n'

export const idorisMessages = defineMessages({
  "en": {
    "title": "Local models (iDoris)",
    "loading": "Checking iDoris…",
    "retry": "Refresh",
    "install": "Download and install",
    "installConsent": "Download the daemon’s locked iDoris release. Local services run with your user permissions; loopback ports do not prove process identity.",
    "connect": "Connect",
    "start": "Start",
    "stop": "Stop",
    "cancel": "Cancel",
    "endpoint": "oMLX endpoint",
    "chooseEndpoint": "Choose an oMLX instance",
    "identity": "Identity: {identity} · Authentication: {authentication}",
    "identity.omlx": "oMLX",
    "identity.unknown_service": "Unconfirmed service",
    "auth.ready": "Ready",
    "auth.auth_required": "Authentication required",
    "auth.auth_rejected": "Key rejected",
    "auth.unreachable": "Unreachable",
    "auth.unknown_service": "Unknown service",
    "auth.invalid_response": "Invalid response",
    "localKey": "Read from local oMLX settings",
    "authHelp": "Open oMLX settings, enable authentication and configure the API key, then refresh here. Agent24 reads it locally after you confirm the endpoint.",
    "noCandidates": "No oMLX instance found. Start oMLX and prepare a model, then refresh.",
    "noModels": "No models available. Prepare a model in oMLX, then refresh.",
    "daily": "Daily model (required)",
    "deep": "Deep model (optional)",
    "chooseModel": "Choose a real model ID",
    "noDeep": "Deep capability is not enabled",
    "core": "Core reserved memory (bytes)",
    "budgetHelp": "Enter the actual allocation for voice, decision and embedding models. No automatic budget recommendation is available. Zero must be explicit. Check the remaining budget with agent24 hw idoris-env --core-reserved-bytes <bytes>.",
    "consent": "I agree to share the selected endpoint’s oMLX key with iDoris, bind the selected models, and issue a local data key stored in Keychain.",
    "saveKey": "Save oMLX key to Keychain",
    "version": "Version: {version}",
    "ports": "Data port: {data} · Admin port: {admin}",
    "roles": "Bound roles: {roles}",
    "unknown": "Unavailable",
    "verified": "Daily chat verified · locality=loopback. Send a test message to see the actual model.",
    "runningHint": "Running. Send a message to check the current chat connection.",
    "chat": "Send a test message",
    "errorGeneric": "Cannot read iDoris state. Check the daemon connection and refresh.",
    "state.not_installed": "Not installed",
    "state.not_configured": "Installed · not connected",
    "state.stopped": "Stopped",
    "state.starting": "Starting",
    "state.running": "Running",
    "state.failed": "Failed",
    "state.instance_conflict": "Instance conflict",
    "phase.queued": "Queued",
    "phase.downloading": "Downloading",
    "phase.verifying": "Verifying download",
    "phase.installing": "Installing",
    "phase.setup": "Configuring models",
    "phase.issuing_key": "Storing data key in Keychain",
    "phase.starting": "Starting iDoris",
    "phase.connecting": "Connecting gateway",
    "phase.verifying_connection": "Verifying daily chat",
    "phase.rolling_back": "Restoring previous configuration",
    "phase.done": "Done",
    "phase.stopping": "Stopping iDoris",
    "phase.completed": "Completed"
  },
  "zh": {
    "title": "本地模型（iDoris）",
    "loading": "正在检测 iDoris…",
    "retry": "刷新",
    "install": "下载并安装",
    "installConsent": "下载 daemon 锁定的 iDoris 版本。服务以当前用户权限运行；loopback 端口不能证明进程身份。",
    "connect": "连接",
    "start": "启动",
    "stop": "停止",
    "cancel": "取消",
    "endpoint": "oMLX 端点",
    "chooseEndpoint": "请选择 oMLX 实例",
    "identity": "身份：{identity} · 认证：{authentication}",
    "identity.omlx": "oMLX",
    "identity.unknown_service": "未确认的服务",
    "auth.ready": "已就绪",
    "auth.auth_required": "需要认证",
    "auth.auth_rejected": "凭据被拒绝",
    "auth.unreachable": "不可达",
    "auth.unknown_service": "未知服务",
    "auth.invalid_response": "响应无效",
    "localKey": "已从本机 oMLX 设置读取",
    "authHelp": "请前往 oMLX 设置开启认证并配置 API key，再回此处刷新。确认端点后 Agent24 会在本机读取。",
    "noCandidates": "未找到 oMLX 实例。请启动 oMLX 并准备模型，再刷新。",
    "noModels": "没有可用模型。请在 oMLX 准备模型后刷新。",
    "daily": "daily 模型（必选）",
    "deep": "deep 模型（可选）",
    "chooseModel": "请选择真实模型 ID",
    "noDeep": "深度能力未启用",
    "core": "core 预留内存（字节）",
    "budgetHelp": "填写语音、决策、嵌入等 core 模型的实际分配。暂未提供自动预算建议；零值须明确输入。可用 agent24 hw idoris-env --core-reserved-bytes <字节> 查看剩余额度。",
    "consent": "我同意将所选端点的 oMLX key 交给 iDoris、绑定所选模型，并签发本机数据 key 存入钥匙串。",
    "saveKey": "保存 oMLX key 到钥匙串",
    "version": "版本：{version}",
    "ports": "数据端口：{data} · Admin 端口：{admin}",
    "roles": "绑定角色：{roles}",
    "unknown": "暂不可用",
    "verified": "已验证 daily 对话 · locality=loopback。发送测试消息可查看实际模型。",
    "runningHint": "正在运行。发送消息可核对当前对话连接。",
    "chat": "发送测试消息",
    "errorGeneric": "无法读取 iDoris 状态。请检查 daemon 连接并刷新。",
    "state.not_installed": "未安装",
    "state.not_configured": "已安装 · 未连接",
    "state.stopped": "已停止",
    "state.starting": "正在启动",
    "state.running": "运行中",
    "state.failed": "故障",
    "state.instance_conflict": "实例冲突",
    "phase.queued": "等待执行",
    "phase.downloading": "下载中",
    "phase.verifying": "校验下载",
    "phase.installing": "安装中",
    "phase.setup": "配置模型",
    "phase.issuing_key": "签发数据 key 并存入钥匙串",
    "phase.starting": "启动 iDoris",
    "phase.connecting": "连接网关",
    "phase.verifying_connection": "验证 daily 对话",
    "phase.rolling_back": "恢复原配置",
    "phase.done": "完成",
    "phase.stopping": "停止 iDoris",
    "phase.completed": "已完成"
  },
  "th": {
    "title": "โมเดลในเครื่อง (iDoris)",
    "loading": "กำลังตรวจสอบ iDoris…",
    "retry": "รีเฟรช",
    "install": "ดาวน์โหลดและติดตั้ง",
    "installConsent": "ดาวน์โหลด iDoris รุ่นที่ daemon กำหนด บริการใช้สิทธิ์ของคุณ พอร์ต loopback ไม่ได้ยืนยันตัวตนของโปรเซส",
    "connect": "เชื่อมต่อ",
    "start": "เริ่ม",
    "stop": "หยุด",
    "cancel": "ยกเลิก",
    "endpoint": "ปลายทาง oMLX",
    "chooseEndpoint": "เลือกอินสแตนซ์ oMLX",
    "identity": "ตัวตน: {identity} · การยืนยันตัวตน: {authentication}",
    "identity.omlx": "oMLX",
    "identity.unknown_service": "บริการที่ยังไม่ยืนยัน",
    "auth.ready": "พร้อม",
    "auth.auth_required": "ต้องยืนยันตัวตน",
    "auth.auth_rejected": "คีย์ถูกปฏิเสธ",
    "auth.unreachable": "เชื่อมต่อไม่ได้",
    "auth.unknown_service": "ไม่รู้จักบริการ",
    "auth.invalid_response": "การตอบกลับไม่ถูกต้อง",
    "localKey": "อ่านจากการตั้งค่า oMLX ในเครื่องแล้ว",
    "authHelp": "เปิดการตั้งค่า oMLX เปิดการยืนยันตัวตนและตั้งค่า API key แล้วรีเฟรชที่นี่ Agent24 จะอ่านคีย์ในเครื่องหลังคุณยืนยันปลายทาง",
    "noCandidates": "ไม่พบ oMLX โปรดเริ่ม oMLX และเตรียมโมเดล แล้วรีเฟรช",
    "noModels": "ไม่มีโมเดล โปรดเตรียมโมเดลใน oMLX แล้วรีเฟรช",
    "daily": "โมเดล daily (จำเป็น)",
    "deep": "โมเดล deep (ไม่บังคับ)",
    "chooseModel": "เลือก ID โมเดลจริง",
    "noDeep": "ยังไม่เปิดความสามารถ deep",
    "core": "หน่วยความจำสำรอง core (ไบต์)",
    "budgetHelp": "ใส่การจัดสรรจริงสำหรับโมเดลเสียง การตัดสินใจ และ embedding ยังไม่มีคำแนะนำงบอัตโนมัติ ต้องระบุศูนย์เอง ตรวจงบที่เหลือด้วย agent24 hw idoris-env --core-reserved-bytes <bytes>",
    "consent": "ฉันยินยอมส่งคีย์ oMLX ของปลายทางที่เลือกให้ iDoris ผูกโมเดลที่เลือก และออกคีย์ข้อมูลในเครื่องเก็บใน Keychain",
    "saveKey": "บันทึกคีย์ oMLX ใน Keychain",
    "version": "เวอร์ชัน: {version}",
    "ports": "พอร์ตข้อมูล: {data} · พอร์ต Admin: {admin}",
    "roles": "บทบาทที่ผูก: {roles}",
    "unknown": "ยังไม่มีข้อมูล",
    "verified": "ตรวจสอบแชต daily แล้ว · locality=loopback ส่งข้อความทดสอบเพื่อดูโมเดลจริง",
    "runningHint": "กำลังทำงาน ส่งข้อความเพื่อตรวจสอบการเชื่อมต่อแชตปัจจุบัน",
    "chat": "ส่งข้อความทดสอบ",
    "errorGeneric": "อ่านสถานะ iDoris ไม่ได้ โปรดตรวจสอบ daemon แล้วรีเฟรช",
    "state.not_installed": "ยังไม่ติดตั้ง",
    "state.not_configured": "ติดตั้งแล้ว · ยังไม่เชื่อมต่อ",
    "state.stopped": "หยุดแล้ว",
    "state.starting": "กำลังเริ่ม",
    "state.running": "กำลังทำงาน",
    "state.failed": "ล้มเหลว",
    "state.instance_conflict": "อินสแตนซ์ขัดแย้ง",
    "phase.queued": "รอคิว",
    "phase.downloading": "กำลังดาวน์โหลด",
    "phase.verifying": "กำลังตรวจสอบไฟล์",
    "phase.installing": "กำลังติดตั้ง",
    "phase.setup": "กำลังตั้งค่าโมเดล",
    "phase.issuing_key": "กำลังเก็บคีย์ข้อมูลใน Keychain",
    "phase.starting": "กำลังเริ่ม iDoris",
    "phase.connecting": "กำลังเชื่อมต่อเกตเวย์",
    "phase.verifying_connection": "กำลังตรวจสอบแชต daily",
    "phase.rolling_back": "กำลังกู้คืนการตั้งค่าเดิม",
    "phase.done": "เสร็จแล้ว",
    "phase.stopping": "กำลังหยุด iDoris",
    "phase.completed": "เสร็จสมบูรณ์"
  }
})

export const idorisErrors = {
  "not_installed": {
    "en": "iDoris is not installed. Download and install it first.",
    "zh": "尚未安装 iDoris，请先下载安装。",
    "th": "ยังไม่ติดตั้ง iDoris โปรดดาวน์โหลดและติดตั้งก่อน"
  },
  "not_configured": {
    "en": "Complete the connection setup before starting iDoris.",
    "zh": "请先完成 iDoris 连接配置。",
    "th": "โปรดตั้งค่าการเชื่อมต่อ iDoris ก่อนเริ่ม"
  },
  "instance_conflict": {
    "en": "Another iDoris instance owns this database. Stop it before retrying.",
    "zh": "此数据库已有 iDoris 实例占用，请先停止该实例再重试。",
    "th": "มี iDoris อีกอินสแตนซ์ใช้ฐานข้อมูลนี้ โปรดหยุดก่อนลองใหม่"
  },
  "keychain_locked": {
    "en": "Unlock Keychain and allow Agent24 access.",
    "zh": "请解锁钥匙串并允许 Agent24 访问。",
    "th": "โปรดปลดล็อก Keychain และอนุญาตให้ Agent24 เข้าถึง"
  },
  "key_unavailable": {
    "en": "The local credential is unavailable. Check Keychain and oMLX settings.",
    "zh": "本机凭据不可用，请检查钥匙串和 oMLX 设置。",
    "th": "ไม่พบข้อมูลรับรองในเครื่อง โปรดตรวจ Keychain และการตั้งค่า oMLX"
  },
  "contract_mismatch": {
    "en": "iDoris identity or version did not match. Check the installed release.",
    "zh": "iDoris 身份或版本契约不匹配，请检查安装版本。",
    "th": "ตัวตนหรือเวอร์ชัน iDoris ไม่ตรง โปรดตรวจรุ่นที่ติดตั้ง"
  },
  "start_failed": {
    "en": "iDoris could not start. Check oMLX and retry.",
    "zh": "iDoris 启动失败，请检查 oMLX 后重试。",
    "th": "เริ่ม iDoris ไม่ได้ โปรดตรวจ oMLX แล้วลองใหม่"
  },
  "stop_timeout": {
    "en": "iDoris stop timed out. Check status before retrying.",
    "zh": "iDoris 停止超时，请核对状态后重试。",
    "th": "หยุด iDoris เกินเวลา โปรดตรวจสถานะก่อนลองใหม่"
  },
  "busy": {
    "en": "Another operation is in progress. Wait for it to finish.",
    "zh": "已有操作正在执行，请等待完成。",
    "th": "มีการดำเนินการอยู่ โปรดรอจนเสร็จ"
  },
  "cancelled": {
    "en": "Operation cancelled.",
    "zh": "操作已取消。",
    "th": "ยกเลิกการดำเนินการแล้ว"
  },
  "not_found": {
    "en": "This operation no longer exists. Refresh status.",
    "zh": "操作不存在，请刷新状态。",
    "th": "ไม่พบการดำเนินการนี้ โปรดรีเฟรชสถานะ"
  },
  "not_cancellable": {
    "en": "This phase cannot be cancelled. Wait for completion.",
    "zh": "当前阶段不可取消，请等待完成。",
    "th": "ยกเลิกขั้นตอนนี้ไม่ได้ โปรดรอจนเสร็จ"
  },
  "shutting_down": {
    "en": "The daemon is shutting down. Reconnect after it restarts.",
    "zh": "daemon 正在关闭，请待重启后重新连接。",
    "th": "daemon กำลังปิด โปรดเชื่อมต่อใหม่หลังเริ่มอีกครั้ง"
  },
  "invalid_request": {
    "en": "The request is invalid. Check the selected endpoint, models and budget.",
    "zh": "请求无效，请核对端点、模型和预算。",
    "th": "คำขอไม่ถูกต้อง โปรดตรวจปลายทาง โมเดล และงบ"
  },
  "persistence_failed": {
    "en": "The daemon could not save the operation. Check disk access.",
    "zh": "daemon 无法保存操作，请检查磁盘访问权限。",
    "th": "daemon บันทึกการดำเนินการไม่ได้ โปรดตรวจสิทธิ์ดิสก์"
  },
  "omlx_unreachable": {
    "en": "Start oMLX locally, then refresh the candidates.",
    "zh": "请启动本机 oMLX 后刷新候选。",
    "th": "โปรดเริ่ม oMLX ในเครื่องแล้วรีเฟรชรายการ"
  },
  "omlx_auth_required": {
    "en": "oMLX requires authentication. Set its key in oMLX settings and retry.",
    "zh": "oMLX 需要认证，请在 oMLX 设置配置 key 后重试。",
    "th": "oMLX ต้องการการยืนยันตัวตน โปรดตั้งค่าคีย์ใน oMLX แล้วลองอีกครั้ง"
  },
  "omlx_auth_rejected": {
    "en": "oMLX rejected the key. Check the key in oMLX settings and retry.",
    "zh": "oMLX 拒绝了凭据，请核对 oMLX 设置中的 key 后重试。",
    "th": "oMLX ปฏิเสธคีย์ โปรดตรวจคีย์ในการตั้งค่า oMLX แล้วลองใหม่"
  },
  "unknown_service": {
    "en": "The selected endpoint is not a confirmed oMLX service.",
    "zh": "所选端点不是已确认的 oMLX 服务。",
    "th": "ปลายทางที่เลือกยังไม่ได้ยืนยันว่าเป็นบริการ oMLX"
  },
  "model_not_found": {
    "en": "The model is missing. Refresh and choose a real oMLX model ID.",
    "zh": "模型不存在，请刷新并选择真实 oMLX 模型 ID。",
    "th": "ไม่พบโมเดล โปรดรีเฟรชและเลือก ID โมเดล oMLX จริง"
  },
  "consent_required": {
    "en": "Confirm credential sharing and the selected models before connecting.",
    "zh": "连接前请同意凭据交接和所选模型。",
    "th": "โปรดยินยอมส่งข้อมูลรับรองและยืนยันโมเดลก่อนเชื่อมต่อ"
  },
  "setup_failed": {
    "en": "Model setup failed. Check oMLX models and the reserved budget, then retry.",
    "zh": "模型配置失败，请检查 oMLX 模型和预留预算后重试。",
    "th": "ตั้งค่าโมเดลล้มเหลว โปรดตรวจโมเดล oMLX และงบสำรองแล้วลองใหม่"
  },
  "verify_failed": {
    "en": "Daily chat verification failed. Check the model and budget before reconnecting.",
    "zh": "daily 对话验证失败，请检查模型和预算后重新连接。",
    "th": "ตรวจสอบแชต daily ไม่ผ่าน โปรดตรวจโมเดลและงบก่อนเชื่อมต่อใหม่"
  },
  "replace_required": {
    "en": "A connection already exists. Keep it and refresh status.",
    "zh": "已有连接，请保留当前连接并刷新状态。",
    "th": "มีการเชื่อมต่อแล้ว โปรดคงไว้และรีเฟรชสถานะ"
  },
  "unauthorized": {
    "en": "The daemon session expired. Reconnect Agent24.",
    "zh": "daemon 会话已失效，请重新连接 Agent24。",
    "th": "เซสชัน daemon หมดอายุ โปรดเชื่อมต่อ Agent24 ใหม่"
  },
  "forbidden": {
    "en": "This daemon session cannot manage local iDoris. Use the host desktop session.",
    "zh": "当前 daemon 会话无权管理本机 iDoris，请使用宿主桌面会话。",
    "th": "เซสชัน daemon นี้จัดการ iDoris ไม่ได้ โปรดใช้เซสชันเดสก์ท็อปของโฮสต์"
  }
} satisfies Record<components['schemas']['IdorisError']['error']['code'], Record<Locale, string>>
