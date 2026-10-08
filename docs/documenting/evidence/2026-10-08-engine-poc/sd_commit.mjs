// Commit simulation: accept only Agent24's revisions (reject one), keep the user's pre-existing ones.
import { SuperDocClient } from '@superdoc/sdk';
const [src, out] = process.argv.slice(2);
const c = new SuperDocClient({ user: { name: 'Agent24' }, env: { SUPERDOC_TELEMETRY: '0' } });
await c.connect();
const d = await c.open({ doc: src });
const list = (await d.trackChanges.list({})).items;
console.log('before:', list.map(t => `${t.author}:${t.type}:${(t.insertedText ?? t.excerpt ?? '').slice(0, 12)}`).join(' | '));
const ours = list.filter(t => t.author === 'Agent24');
const reject = ours.find(t => (t.insertedText ?? t.excerpt ?? '').includes('陈静'));
const accept = ours.filter(t => t !== reject).map(t => t.id);
await d.trackChanges.decide({ decision: 'accept', target: { kind: 'ids', ids: accept } });
if (reject) await d.trackChanges.decide({ decision: 'reject', target: { kind: 'id', id: reject.id } });
const after = (await d.trackChanges.list({})).items;
console.log('after:', after.map(t => `${t.author}:${t.type}:${(t.insertedText ?? t.excerpt ?? '').slice(0, 12)}`).join(' | '));
await d.save({ out, mode: 'review-preserving' });
await d.close(); await c.dispose();
