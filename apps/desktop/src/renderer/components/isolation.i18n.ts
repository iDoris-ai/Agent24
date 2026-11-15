import { defineMessages } from '../i18n'

export const isolationMessages = defineMessages({
  en: {
    isolated: 'Isolated',
    platform: 'Not isolated · Isolation is not available on this platform',
    form: 'Not isolated · This kind of component is not isolated',
  },
  zh: {
    isolated: '已隔离',
    platform: '未隔离 · 此平台暂不支持隔离',
    form: '未隔离 · 此类组件暂不隔离',
  },
  th: {
    isolated: 'แยกส่วนแล้ว',
    platform: 'ยังไม่แยกส่วน · แพลตฟอร์มนี้ยังไม่รองรับการแยกส่วน',
    form: 'ยังไม่แยกส่วน · ส่วนประกอบประเภทนี้ยังไม่ถูกแยกส่วน',
  },
})
