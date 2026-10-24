import { defineMessages } from '../../i18n'

export const cronPreviewMessages = defineMessages({
  en: {
    'every.range': 'Interval seconds must be {min}–{max}',
    'at.invalid': 'Invalid time format (ISO 8601 required)',
    'at.past': 'That time has already passed',
    'cron.empty': 'Enter a cron expression',
    'cron.invalid': 'Invalid cron expression',
    'cron.noFire': 'Does not fire within a year (the server will compute the next time)',
    'unknownType': 'Unknown schedule type',
  },
  zh: {
    'every.range': '每隔秒数须为 {min}–{max}',
    'at.invalid': '时间格式无效（需 ISO 8601）',
    'at.past': '该时间已过',
    'cron.empty': '请输入 cron 表达式',
    'cron.invalid': 'cron 表达式无效',
    'cron.noFire': '一年内不触发（服务端将计算下次时间）',
    'unknownType': '未知调度类型',
  },
  th: {
    'every.range': 'ช่วงเวลาต้องอยู่ระหว่าง {min}–{max} วินาที',
    'at.invalid': 'รูปแบบเวลาไม่ถูกต้อง (ต้องเป็น ISO 8601)',
    'at.past': 'เวลานี้ผ่านไปแล้ว',
    'cron.empty': 'โปรดกรอกนิพจน์ cron',
    'cron.invalid': 'นิพจน์ cron ไม่ถูกต้อง',
    'cron.noFire': 'ไม่มีรอบทำงานใน 1 ปีข้างหน้า (บริการเบื้องหลังจะคำนวณเวลาครั้งถัดไปเอง)',
    'unknownType': 'ไม่ทราบประเภทกำหนดการ',
  },
})
