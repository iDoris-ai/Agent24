import { defineMessages } from '../i18n'

export const privacyMessages = defineMessages({
  en: {
    local: 'Local',
    remote: 'Remote',
    'local.title': 'Processed on this computer; nothing left it',
    'remote.title': 'Sent to a remote model',
  },
  zh: {
    local: '本地',
    remote: '远端',
    'local.title': '在本机处理，没有离开这台电脑',
    'remote.title': '发往远端模型处理',
  },
  th: {
    local: 'ในเครื่อง',
    remote: 'ระยะไกล',
    'local.title': 'ประมวลผลบนเครื่องนี้ ข้อมูลไม่ได้ออกจากเครื่อง',
    'remote.title': 'ส่งไปยังโมเดลระยะไกล',
  },
})
