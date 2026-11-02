import { defineMessages } from '../i18n'

export const serviceBoxDemoMessages = defineMessages({
  en: {
    'title': 'Service container demo', 'status.checking': 'Checking…',
    'status.running': '● Running :{port}', 'status.starting': '● Container starting…',
    'subtitle': 'BoxLite persistent service container · python:slim · Requests proxied through /api/svc/*',
    'startup.hint': 'The container is starting. The first launch needs to pull the python:slim image (about 1–2 minutes)…',
    'echo.title': 'Echo API test', 'echo.defaultMessage': 'Hello from Agent24!',
    'echo.placeholder': 'Enter a message…', 'echo.calling': 'Calling…', 'echo.call': '▶ Call',
    'echo.response': 'Response', 'echo.empty': 'Click “▶ Call” to send a request to the container service',
    'echo.error': 'Error: {error}', 'route.path': 'Route path:', 'image.label': 'Container image:',
    'image.port': 'port', 'api.available': 'Available endpoints:',
  },
  zh: {
    'title': '服务容器示例', 'status.checking': '检测中…',
    'status.running': '● 运行中 :{port}', 'status.starting': '● 容器启动中…',
    'subtitle': 'BoxLite 长期服务容器 · python:slim · 请求通过 /api/svc/* 代理',
    'startup.hint': '容器正在启动，首次需拉取 python:slim 镜像（约 1-2 分钟）…',
    'echo.title': 'Echo 接口测试', 'echo.defaultMessage': '来自 Agent24 的问候！',
    'echo.placeholder': '输入消息…', 'echo.calling': '调用中…', 'echo.call': '▶ 调用',
    'echo.response': '响应', 'echo.empty': '点击"▶ 调用"发送请求到容器服务',
    'echo.error': '错误：{error}', 'route.path': '路由路径：', 'image.label': '容器镜像：',
    'image.port': '端口', 'api.available': '可用接口：',
  },
  th: {
    'title': 'ตัวอย่างบริการในคอนเทนเนอร์', 'status.checking': 'กำลังตรวจสอบ…',
    'status.running': '● กำลังทำงาน :{port}', 'status.starting': '● กำลังเริ่มคอนเทนเนอร์…',
    'subtitle': 'คอนเทนเนอร์บริการถาวร BoxLite · python:slim · ส่งคำขอผ่าน /api/svc/*',
    'startup.hint': 'กำลังเริ่มคอนเทนเนอร์ ครั้งแรกต้องดาวน์โหลดอิมเมจ python:slim (ประมาณ 1–2 นาที)…',
    'echo.title': 'ทดสอบ Echo API', 'echo.defaultMessage': 'สวัสดีจาก Agent24!',
    'echo.placeholder': 'พิมพ์ข้อความ…', 'echo.calling': 'กำลังส่ง…', 'echo.call': '▶ ส่ง',
    'echo.response': 'การตอบกลับ', 'echo.empty': 'คลิก “▶ ส่ง” เพื่อส่งคำขอไปยังบริการในคอนเทนเนอร์',
    'echo.error': 'ข้อผิดพลาด: {error}', 'route.path': 'เส้นทาง:', 'image.label': 'อิมเมจคอนเทนเนอร์:',
    'image.port': 'พอร์ต', 'api.available': 'ปลายทางที่ใช้ได้:',
  },
})
