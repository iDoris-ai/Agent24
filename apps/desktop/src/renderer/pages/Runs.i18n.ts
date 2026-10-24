import { defineMessages } from '../i18n'

export const runsMessages = defineMessages({
  en: {
    'title': 'Runs', 'subtitle': 'Agent run history · Refreshes every 3 seconds',
    'status.queued': 'Queued', 'status.running': 'Running', 'status.awaitingApproval': 'Awaiting approval',
    'status.completed': 'Completed', 'status.failed': 'Failed', 'status.cancelled': 'Cancelled',
    'list.empty': 'No runs yet', 'list.scheduled': 'Triggered by a schedule',
    'detail.select': 'Select a run to view details', 'detail.cancel': 'Cancel',
    'detail.prompt': 'Prompt:', 'detail.output': 'Output:', 'detail.error': 'Error [{code}]:',
    'detail.created': 'Created {date}', 'detail.ended': 'Ended {date}',
    'usage.short': '{count} tok', 'usage.full': '{count} tokens',
  },
  zh: {
    'title': '运行任务', 'subtitle': 'Agent 运行历史 · 每 3 秒刷新',
    'status.queued': '排队', 'status.running': '运行中', 'status.awaitingApproval': '待审批',
    'status.completed': '已完成', 'status.failed': '失败', 'status.cancelled': '已取消',
    'list.empty': '暂无运行任务', 'list.scheduled': '由调度触发',
    'detail.select': '选择一个任务查看详情', 'detail.cancel': '取消',
    'detail.prompt': 'Prompt:', 'detail.output': '输出:', 'detail.error': '错误 [{code}]:',
    'detail.created': '创建 {date}', 'detail.ended': '结束 {date}',
    'usage.short': '{count} tok', 'usage.full': '{count} tokens',
  },
  th: {
    'title': 'งานที่รัน', 'subtitle': 'ประวัติการทำงานของ Agent · อัปเดตทุก 3 วินาที',
    'status.queued': 'รอคิว', 'status.running': 'กำลังทำงาน', 'status.awaitingApproval': 'รออนุมัติ',
    'status.completed': 'เสร็จแล้ว', 'status.failed': 'ล้มเหลว', 'status.cancelled': 'ยกเลิกแล้ว',
    'list.empty': 'ยังไม่มีงานที่รัน', 'list.scheduled': 'เริ่มจากกำหนดเวลา',
    'detail.select': 'เลือกงานเพื่อดูรายละเอียด', 'detail.cancel': 'ยกเลิก',
    'detail.prompt': 'คำสั่ง:', 'detail.output': 'ผลลัพธ์:', 'detail.error': 'ข้อผิดพลาด [{code}]:',
    'detail.created': 'สร้างเมื่อ {date}', 'detail.ended': 'สิ้นสุดเมื่อ {date}',
    'usage.short': '{count} โทเค็น', 'usage.full': '{count} โทเค็น',
  },
})
