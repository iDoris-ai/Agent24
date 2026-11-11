import { defineMessages } from './i18n'

// M5 B4c2: Settings → General → background service. `label` is named in the
// first-launch box's detail (main-i18n `consent.detail`), so the two read the same.
export const residentServiceMessages = defineMessages({
  en: {
    title: 'Background service',
    label: 'Run in the background and start at login',
    hint: 'Agent24 keeps working after you close its window. Turning this off asks you to confirm, then stops the background service.',
    busy: 'Applying…',
    'failed.install': 'Could not turn on the background service ({code}). Agent24 runs inside this app.',
    'failed.start': 'Could not turn on the background service ({code}). Agent24 runs inside this app.',
    'failed.shutdown': 'The background service was not stopped ({code}). Nothing was changed.',
    'failed.not_connected': 'Agent24 is not connected to the background service, so it cannot ask to stop it. Choose “Start daemon” in the tray menu to connect, then turn this off. If it does not connect, run `agent24 service uninstall`.',
    'failed.uninstall': 'The background service stopped, but its login item could not be removed ({code}). Run `agent24 service uninstall`.',
    'failed.unavailable': 'This needs the packaged macOS app.',
    'failed.busy': 'Another change is still being applied. Try again when it finishes.',
  },
  zh: {
    title: '后台服务',
    label: '在后台运行并随登录启动',
    hint: '关掉窗口后 Agent24 仍会继续工作。关闭这一项会先请你确认，然后停止后台服务。',
    busy: '正在应用…',
    'failed.install': '没能开启后台服务（{code}），仍由本应用运行 daemon。',
    'failed.start': '没能开启后台服务（{code}），仍由本应用运行 daemon。',
    'failed.shutdown': '后台服务没有停止（{code}），什么都没改。',
    'failed.not_connected': 'Agent24 没有连着后台服务，没法请求停止它。请先在托盘菜单里点「启动 daemon」连上，再关闭这一项。连不上的话，运行 `agent24 service uninstall`。',
    'failed.uninstall': '后台服务已停止，但没能移除登录启动项（{code}）。请运行 `agent24 service uninstall`。',
    'failed.unavailable': '需要 macOS 正式版应用。',
    'failed.busy': '上一次改动还没做完，请等它完成后再试。',
  },
  th: {
    title: 'บริการเบื้องหลัง',
    label: 'ทำงานเบื้องหลังและเริ่มเมื่อเข้าสู่ระบบ',
    hint: 'Agent24 จะทำงานต่อหลังจากปิดหน้าต่าง การปิดตัวเลือกนี้จะขอให้คุณยืนยันก่อน แล้วจึงหยุดบริการเบื้องหลัง',
    busy: 'กำลังใช้การตั้งค่า…',
    'failed.install': 'เปิดบริการเบื้องหลังไม่สำเร็จ ({code}) Agent24 จะทำงานภายในแอปนี้',
    'failed.start': 'เปิดบริการเบื้องหลังไม่สำเร็จ ({code}) Agent24 จะทำงานภายในแอปนี้',
    'failed.shutdown': 'บริการเบื้องหลังยังไม่หยุด ({code}) ไม่มีการเปลี่ยนแปลงใด',
    'failed.not_connected': 'Agent24 ไม่ได้เชื่อมต่อกับบริการเบื้องหลัง จึงขอให้หยุดไม่ได้ ให้เลือก "เริ่ม daemon" ในเมนูถาดเพื่อเชื่อมต่อก่อน แล้วจึงปิดตัวเลือกนี้ หากเชื่อมต่อไม่ได้ ให้รัน `agent24 service uninstall`',
    'failed.uninstall': 'บริการเบื้องหลังหยุดแล้ว แต่ลบรายการเริ่มเมื่อเข้าสู่ระบบไม่สำเร็จ ({code}) ให้รัน `agent24 service uninstall`',
    'failed.unavailable': 'ต้องใช้แอป macOS รุ่นทางการ',
    'failed.busy': 'การเปลี่ยนแปลงก่อนหน้ายังไม่เสร็จ โปรดลองอีกครั้งเมื่อเสร็จแล้ว',
  },
})
