import { defineMessages } from '../i18n'

export const approvalsMessages = defineMessages({
  en: {
    'page.title': 'Pending approvals ({count})',
    'page.subtitle': 'Tool execution approvals · Refreshes every 2 seconds · New approvals trigger a system notification',
    'notification.title': 'Approval required',
    'empty': 'No pending approvals',
    'decision.approve': 'Approve', 'decision.approveForSession': 'Approve for session',
    'decision.deny': 'Deny', 'decision.abort': 'Abort run',
    'deny.reason': 'Reason for denial', 'deny.placeholder': 'Reason for denial (required)',
    'deny.confirm': 'Confirm denial', 'deny.back': 'Back',
  },
  zh: {
    'page.title': '待审批 ({count})',
    'page.subtitle': '工具执行审批 · 每 2 秒刷新 · 新审批弹系统通知',
    'notification.title': '需要审批',
    'empty': '没有待处理的审批',
    'decision.approve': '批准', 'decision.approveForSession': '本会话免问',
    'decision.deny': '拒绝', 'decision.abort': '中止运行',
    'deny.reason': '拒绝原因', 'deny.placeholder': '拒绝原因（必填）',
    'deny.confirm': '确认拒绝', 'deny.back': '返回',
  },
  th: {
    'page.title': 'รอการอนุมัติ ({count})',
    'page.subtitle': 'อนุมัติการใช้เครื่องมือ · รีเฟรชทุก 2 วินาที · แจ้งเตือนเมื่อมีรายการใหม่',
    'notification.title': 'ต้องอนุมัติ',
    'empty': 'ไม่มีรายการรออนุมัติ',
    'decision.approve': 'อนุมัติ', 'decision.approveForSession': 'อนุมัติตลอดเซสชัน',
    'decision.deny': 'ปฏิเสธ', 'decision.abort': 'ยกเลิกการทำงาน',
    'deny.reason': 'เหตุผลที่ปฏิเสธ', 'deny.placeholder': 'เหตุผลที่ปฏิเสธ (จำเป็น)',
    'deny.confirm': 'ยืนยันการปฏิเสธ', 'deny.back': 'กลับ',
  },
})
