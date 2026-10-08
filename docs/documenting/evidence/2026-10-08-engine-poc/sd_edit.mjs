// Pipeline A: SuperDoc headless SDK — same 6 operations as the LibreOffice pipeline.
import { SuperDocClient } from '@superdoc/sdk';
import { readFileSync } from 'node:fs';
const [src, out, mode] = process.argv.slice(2);
const changeMode = mode === 'tracked' ? 'tracked' : 'direct';
const log = [];
const step = async (label, f) => { try { const r = await f(); log.push(`${label}: ok ${JSON.stringify(r?.receipt ?? r?.success ?? r?.status ?? '').slice(0, 160)}`); return r; } catch (e) { log.push(`${label}: ERR ${String(e).slice(0, 300)}`); } };
const t0 = Date.now();
const c = new SuperDocClient({ user: { name: 'Agent24' }, env: { SUPERDOC_TELEMETRY: '0' } });
await c.connect();
const t1 = Date.now();
const d = await c.open({ doc: src });
const t2 = Date.now();
// 1. bookmark target: replace the bookmarked range, guarded by expected text
const bm = await d.bookmarks.get({ target: { kind: 'entity', entityType: 'bookmark', name: 'deadline' } });
await step('bookmark', () => d.replace({ changeMode, target: { kind: 'selection', start: { kind: 'text', ...bm.range.from }, end: { kind: 'text', ...bm.range.to } }, text: '2026年11月5日' }));
// 2. content control
const cc = (await d.contentControls.selectByTag({ tag: 'contact' })).items[0];
await step('contentControl', () => d.contentControls.text.setValue({ changeMode, target: cc.target, value: '刘老师 021-87654321', text: '刘老师 021-87654321' }));
// 3. table cell (row 2, col 3 is "待定" in the logical grid: rowIndex 2, columnIndex 3)
const tbl = (await d.find({ select: { type: 'node', nodeType: 'table' }, limit: 1 })).items[0].address;
await step('cell', () => d.tables.setCellText({ changeMode, target: tbl, rowIndex: 2, columnIndex: 3, text: '陈静' }));
// 4. append row cloned from last row, then fill
await step('insertRow', () => d.tables.insertRow({ changeMode, target: tbl, rowIndex: 3, position: 'below' }));
for (const [ci, v] of ['10月27日', '上午', '9:00-11:00', '赵磊'].entries())
  await step(`fill r4c${ci}`, () => d.tables.setCellText({ changeMode, target: tbl, rowIndex: 4, columnIndex: ci, text: v }));
// 5. list item after "陪伴独居老人"
const li = (await d.lists.list({})).items.find(i => i.text === '陪伴独居老人');
await step('listInsert', () => d.lists.insert({ changeMode, target: li.address, position: 'after', text: '社区图书整理' }));
// 6. image after the table
const png = 'data:image/png;base64,' + readFileSync(process.argv[5]).toString('base64');
await step('image', () => d.create.image({ changeMode, src: png, size: { width: 200, height: 100 }, at: { kind: 'after', target: tbl } }));
const t3 = Date.now();
await step('save', () => d.save({ out, mode: 'review-preserving' }));
const t4 = Date.now();
await d.close(); await c.dispose();
console.log(log.join('\n'));
console.log(`connect ${t1 - t0}ms open ${t2 - t1}ms edits ${t3 - t2}ms save ${t4 - t3}ms`);
