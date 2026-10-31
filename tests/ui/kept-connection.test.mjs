// Which connection a tab's work goes to. A tab's runs share a connection of its own, kept between
// them, so @variables and temporary tables carry from one run to the next; with Auto-commit off the
// same connection is the tab's transaction. What is not a run - Apply with Auto-commit on, reading a
// whole result, a plan - goes elsewhere, so it never waits for a result still being read there.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// NOBS_UI_SOURCE lets the PowerShell edition run this same file against NOBSSQL.ps1, which carries
// the identical UI inline.
const html = readFileSync(process.env.NOBS_UI_SOURCE ||
  join(dirname(fileURLToPath(import.meta.url)), '../../ui/index.html'), 'utf8').replace(/\r\n/g, '\n');
const skip = html.includes('function runSessOf(') ? false : 'this edition does not keep a tab\'s connection yet';

function extractFunction(src, name) {
  let start = src.indexOf(`async function ${name}(`);
  if (start === -1) start = src.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `function ${name} not found - was it renamed?`);
  let depth = 0;
  for (let j = src.indexOf('{', start); j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`unbalanced braces while extracting ${name}`);
}

const NAMES = ['sessOf', 'keepConnOn', 'setKeepConn', 'tabSess', 'runSessOf', 'tabSessClose', 'tabQuery', 'sessReset',
  'txToggle', 'txClose', 'sessFree', 'api'];
const bundle = skip ? '' : NAMES.map(n => extractFunction(html, n)).join('\n') + `\nreturn {${NAMES.join(',')}};`;

// A page with some tabs, a server that answers from `answer`, and every request kept.
function page({ tabs = [], answer = () => ({ ok: true }), keep } = {}) {
  const store = new Map(keep === undefined ? [] : [['keepConn', keep ? 'on' : 'off']]);
  const sent = [], toasts = [], logs = [];
  const win = { browseCharset: '' };
  const env = {
    tabs, window: win, T: id => tabs.find(t => t.id === id),
    localStorage: { getItem: k => (store.has(k) ? store.get(k) : null), setItem: (k, v) => store.set(k, String(v)) },
    apiCall: async (path, p) => { sent.push({ path, ...p }); return answer(path, p); },
    txWatch: () => null, txLost: () => {}, TX_LOST: /holding this tab's transaction was lost/,
    txPaint: () => {}, closeCursorFor: t => { if (t.cursorId) sent.push({ path: '/api/close-cursor', cursorId: t.cursorId }); t.cursorId = null; },
    updateStatusLine: () => {}, toast: (m, k) => toasts.push((k === true ? 'ERR ' : '') + m), log: m => logs.push(m),
  };
  const keys = Object.keys(env);
  const f = new Function(...keys, bundle)(...keys.map(k => env[k]));
  return { f, sent, toasts, logs, win, store };
}
const tab = (o = {}) => ({ id: 't1', title: 'Query', txOn: false, txSession: null, sessId: null, cursorId: null, ...o });

test('with Auto-commit on, a tab\'s runs share one kept connection', { skip }, () => {
  const t = tab();
  const { f } = page({ tabs: [t] });
  const s = f.runSessOf(t);
  assert.match(s, /^tab_t1_/);
  assert.equal(f.runSessOf(t), s, 'the same one every time');
  assert.equal(f.sessOf(t), undefined, 'and it is not a transaction: Apply does not go there');
});

test('switched off in Settings, or reading another charset, a run gets a connection of its own', { skip }, () => {
  const t = tab();
  assert.equal(page({ tabs: [t], keep: false }).f.runSessOf(t), undefined);
  const p = page({ tabs: [t] }); p.win.browseCharset = 'latin1';
  assert.equal(p.f.runSessOf(t), undefined);
});

test('Auto-commit off makes the kept connection the transaction, and on again keeps it', { skip }, async () => {
  const t = tab();
  const { f, sent } = page({ tabs: [t] });
  const s = f.runSessOf(t);
  f.txToggle('t1', true);
  assert.equal(t.txSession, s, 'the transaction is the connection the runs already had');
  assert.equal(f.runSessOf(t), s); assert.equal(f.sessOf(t), s);
  t.cursorId = 'c1';
  f.txToggle('t1', false);
  await new Promise(r => setTimeout(r, 0));
  assert.equal(t.txOn, false); assert.equal(t.txSession, null); assert.equal(t.sessId, s, 'the connection is kept');
  assert.deepEqual(sent.map(x => [x.path, x.action || x.cursorId]), [['/api/close-cursor', 'c1'], ['/api/session-end', 'release']],
    'the result still being read lets go of it first, then the transaction ends and the connection stays');
});

test('a transaction with something to commit is not ended by turning Auto-commit on', { skip }, () => {
  const t = tab();
  const { f, sent, toasts } = page({ tabs: [t] });
  f.txToggle('t1', true); t.txDirty = true;
  f.txToggle('t1', false);
  assert.equal(t.txOn, true); assert.deepEqual(sent, []);
  assert.ok(toasts.some(m => /Commit or roll back/.test(m)));
});

test('with keeping off, ending the transaction lets the connection go, as before', { skip }, () => {
  const t = tab();
  const { f, sent } = page({ tabs: [t], keep: false });
  f.txToggle('t1', true);
  const s = t.txSession;
  assert.ok(s, 'a transaction still gets its connection');
  f.txToggle('t1', false);
  assert.equal(t.sessId, null);
  assert.deepEqual(sent.map(x => [x.path, x.session, x.action]), [['/api/session-end', s, 'close']]);
});

test('switching keeping off lets the kept connections go, but not a transaction\'s', { skip }, () => {
  const a = tab({ id: 'a' }), b = tab({ id: 'b' });
  const { f, sent, store } = page({ tabs: [a, b] });
  f.runSessOf(a); f.txToggle('b', true);
  const bs = b.txSession;
  f.setKeepConn(false);
  assert.equal(store.get('keepConn'), 'off');
  assert.equal(a.sessId, null); assert.equal(b.sessId, bs);
  assert.deepEqual(sent.map(x => x.action), ['close']);
});

test('each request on a tab\'s connection says whether it is in the transaction', { skip }, async () => {
  const t = tab();
  const { f, sent } = page({ tabs: [t] });
  const s = f.runSessOf(t);
  await f.api('/api/query', { sql: 'SELECT 1', session: s });
  f.txToggle('t1', true);
  await f.api('/api/query', { sql: 'SELECT 1', session: s });
  await f.api('/api/query', { sql: 'SELECT 1', session: 'from_elsewhere' });
  assert.deepEqual(sent.map(x => x.sessionTx), [false, true, undefined], 'a session no tab knows keeps the backend\'s default');
});

test('a connection the server had closed is opened again, and the user is told', { skip }, async () => {
  const t = tab();
  const { f, toasts } = page({ tabs: [t], answer: () => ({ ok: true, rows: [], sessionReset: true }) });
  await f.api('/api/query', { sql: 'SELECT @x', session: f.runSessOf(t) });
  assert.ok(toasts.some(m => /connection had been closed.*@variables, temporary tables/.test(m)), toasts.join());
});

test('a plan or a whole result runs on its own connection, and on the tab\'s only when it needs to', { skip }, async () => {
  const t = tab({ cursorId: 'cur' });
  let fail = true;
  const { f, sent } = page({ tabs: [t], answer: (path, p) => (path === '/api/query' && !p.session && fail
    ? { ok: false, error: "Table 'd.tmp' doesn't exist" } : { ok: true, rows: [['x']] }) });
  f.runSessOf(t);
  const r = await f.tabQuery(t, { sql: 'EXPLAIN SELECT * FROM tmp', db: 'd' });
  assert.ok(r.ok);
  assert.deepEqual(sent.map(x => [x.path, x.session ? 'tab' : 'own']),
    [['/api/query', 'own'], ['/api/close-cursor', 'own'], ['/api/query', 'tab']],
    'a temporary table is only on the tab\'s connection, whose open result is closed first');
  sent.length = 0; fail = false; t.cursorId = 'cur2';
  await f.tabQuery(t, { sql: 'EXPLAIN SELECT 1', db: 'd' });
  assert.deepEqual(sent.map(x => x.path), ['/api/query'], 'when its own connection will do, the result being read is left alone');
});

test('another failure is the answer, not a reason to try the tab\'s connection', { skip }, async () => {
  const t = tab();
  const { f, sent } = page({ tabs: [t], answer: () => ({ ok: false, error: 'You have an error in your SQL syntax' }) });
  f.runSessOf(t);
  const r = await f.tabQuery(t, { sql: 'EXPLAIN SELEC', db: 'd' });
  assert.equal(r.ok, false); assert.equal(sent.length, 1);
});

test('in a transaction, a plan goes into the transaction', { skip }, async () => {
  const t = tab();
  const { f, sent } = page({ tabs: [t] });
  f.txToggle('t1', true);
  await f.tabQuery(t, { sql: 'EXPLAIN SELECT 1', db: 'd' });
  assert.deepEqual(sent.map(x => [x.session, x.sessionTx]), [[t.txSession, true]]);
});
