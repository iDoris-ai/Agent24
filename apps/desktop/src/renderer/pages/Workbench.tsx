const CAPABILITIES = [
  { icon: '🎙️', name: 'ASR 语音识别', desc: '上传音频/视频，自动转文字字幕', status: 'coming', tag: 'mlx-whisper' },
  { icon: '🔊', name: 'TTS 语音合成', desc: '文字转自然语音，支持中英多语种', status: 'coming', tag: 'Kokoro / CosyVoice2' },
  { icon: '🖼️', name: '文生图', desc: 'FLUX.1-schnell，本地 Apple Silicon 加速', status: 'coming', tag: 'mflux' },
  { icon: '🎬', name: '视频生成', desc: 'LTX-Video 短视频生成，独占内存模式', status: 'coming', tag: 'LTX-2.3 Q4' },
  { icon: '📄', name: 'OCR 文档识别', desc: 'PDF/图片 → Markdown，支持表格', status: 'coming', tag: 'GOT-OCR2' },
  { icon: '🌐', name: '翻译', desc: '复用主力 LLM，100+ 语种互译', status: 'ready', tag: 'Qwen3-30B' },
  { icon: '🕷️', name: '网页抓取', desc: '结构化内容提取，送 LLM 摘要', status: 'coming', tag: 'Crawl4AI' },
  { icon: '🚀', name: '社媒发布', desc: '小红书 / 公众号 / 微博自动发布', status: 'coming', tag: 'Playwright' },
  { icon: '🔍', name: 'RAG 知识库', desc: 'BGE-M3 嵌入 + 向量检索 + 重排序', status: 'coming', tag: 'ChromaDB' },
]

export default function WorkbenchPage() {
  return (
    <div className="content">
      <div className="page-title">工作台</div>
      <div className="page-sub">选择能力模块执行任务，更多模块即将上线</div>
      <div className="workbench-grid">
        {CAPABILITIES.map(cap => (
          <div key={cap.name} className="capability-card">
            <div className="cap-icon">{cap.icon}</div>
            <h3>{cap.name}</h3>
            <p>{cap.desc}</p>
            <div className={`cap-status ${cap.status === 'ready' ? 'ready' : ''}`}>
              <span className="tag">{cap.tag}</span>
              {' '}
              {cap.status === 'ready' ? '✓ 可用' : '开发中'}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
