import { defineMessages } from '../../i18n'

export const docxToolsMessages = defineMessages({
  en: {
    'fill.title': 'Fill in template', 'fill.load': 'Fill in template…', 'fill.none': 'This document has no {{field}} placeholders.',
    'fill.hint': 'Fields are written as {{name}} in Word or WPS. The template is not changed; a new document is made.',
    'fill.make': 'Make document', 'fill.empty': 'Fill in at least one field.',
    'patch.title': 'Rewrite paragraphs', 'patch.load': 'Rewrite paragraphs…', 'patch.pick': 'Paragraph',
    'patch.tracked': 'As tracked changes (accept or reject in Word/WPS)', 'patch.make': 'Make revised document',
    'patch.same': 'Change the paragraph text first.',
    'busy': 'Working…', 'done': 'Made 「{title}」 in the document library.', 'failed': 'Could not make it: {error}',
    'save': 'Save to Downloads', 'saved': 'Saved: Downloads/{name}', 'reveal': 'Show in Finder', 'saveFailed': 'Could not save: {error}',
    'close': 'Close',
  },
  zh: {
    'fill.title': '填写模板', 'fill.load': '填写模板…', 'fill.none': '这份文档里没有 {{字段}} 占位符。',
    'fill.hint': '在 Word 或 WPS 里把要填的地方写成 {{字段名}}。模板本身不变，会生成一份新文档。',
    'fill.make': '生成文档', 'fill.empty': '至少填写一个字段。',
    'patch.title': '改写段落', 'patch.load': '改写段落…', 'patch.pick': '段落',
    'patch.tracked': '以修订方式写入（可在 Word/WPS 里接受或拒绝）', 'patch.make': '生成修订版',
    'patch.same': '请先修改段落文字。',
    'busy': '正在生成…', 'done': '已生成「{title}」，在文档库中。', 'failed': '没能生成：{error}',
    'save': '保存到下载', 'saved': '已保存：下载/{name}', 'reveal': '在 Finder 中显示', 'saveFailed': '没能保存：{error}',
    'close': '收起',
  },
  th: {
    'fill.title': 'กรอกแม่แบบ', 'fill.load': 'กรอกแม่แบบ…', 'fill.none': 'เอกสารนี้ไม่มีช่อง {{field}}',
    'fill.hint': 'เขียนช่องเป็น {{ชื่อ}} ใน Word หรือ WPS แม่แบบไม่ถูกแก้ไข จะสร้างเอกสารใหม่',
    'fill.make': 'สร้างเอกสาร', 'fill.empty': 'กรอกอย่างน้อยหนึ่งช่อง',
    'patch.title': 'แก้ย่อหน้า', 'patch.load': 'แก้ย่อหน้า…', 'patch.pick': 'ย่อหน้า',
    'patch.tracked': 'บันทึกเป็นการติดตามการแก้ไข', 'patch.make': 'สร้างฉบับแก้ไข',
    'patch.same': 'แก้ข้อความของย่อหน้าก่อน',
    'busy': 'กำลังสร้าง…', 'done': 'สร้าง「{title}」ในคลังเอกสารแล้ว', 'failed': 'สร้างไม่ได้: {error}',
    'save': 'บันทึกไปที่ดาวน์โหลด', 'saved': 'บันทึกแล้ว: ดาวน์โหลด/{name}', 'reveal': 'แสดงใน Finder', 'saveFailed': 'บันทึกไม่ได้: {error}',
    'close': 'ปิด',
  },
})
