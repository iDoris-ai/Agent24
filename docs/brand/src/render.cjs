// 把 docs/brand/*.svg 渲染成透明底 PNG（用 Playwright 的 Chromium）。
// 用法：node docs/brand/src/render.cjs <svg> <out.png> <px>
const { chromium } = require('playwright') // 仓库未装时用 NODE_PATH 指向任一 playwright 安装
const { readFileSync } = require('node:fs')
const [svgPath, out, px] = process.argv.slice(2)
const size = Number(px)
const svg = readFileSync(svgPath, 'utf8').replace(/width="\d+" height="\d+"/, `width="${size}" height="${size}"`)
;(async () => {
const browser = await chromium.launch(process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {})
const page = await browser.newPage({ viewport: { width: size, height: size } })
await page.setContent(`<html><body style="margin:0;background:transparent">${svg}</body></html>`)
await page.screenshot({ path: out, omitBackground: true })
await browser.close()
})()
