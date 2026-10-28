// How Apply finds the rows it changes or deletes.
//
// Apply matched a row by its key as the grid shows it and then reported success whatever that
// matched. A FLOAT key is shown rounded, so its edits matched nothing and were reported as applied.
// A TIMESTAMP key is shown in the session time zone, where the autumn hour happens twice, so
// editing one of two such rows changed the other. Each change is now preceded by a guard that stops
// the whole batch unless its row is matched exactly once.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// NOBS_UI_SOURCE lets the PowerShell edition run this same file against NOBSSQL.ps1, which carries
// the identical UI inline.
const html = readFileSync(process.env.NOBS_UI_SOURCE ||
  join(dirname(fileURLToPath(import.meta.url)), '../../ui/index.html'), 'utf8').replace(/\r\n/g, '\n');

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
function extractConst(src, name) {
  const start = src.indexOf(`const ${name}=`);
  assert.notEqual(start, -1, `const ${name} not found - was it renamed?`);
  return src.slice(start, src.indexOf(';\n', start) + 1);
}

const NAMES = ['applyChanges', 'applyChangesRun', 'keyWhere', 'oneRowGuard', 'oneRowGuardMany', 'litAs', 'lit', 'strLit',
  'pastedHexColumns', 'looksLikePastedHex', 'normalizeHexInput', 'splitStmts', 'sqlHead'];
// How Apply reads the server's warnings. An edition without them yet gets stand-ins that find none.
const hasWarnings = html.includes('function warnFromWrites(');
const WARN_NAMES = ['warnIsNote', 'warnText', 'warnLine', 'warnFromWrites'];
const bundle = extractConst(html, 'ONE_ROW_REFUSED') + '\n' + NAMES.map(n => extractFunction(html, n)).join('\n') + '\n' +
  (hasWarnings ? WARN_NAMES.map(n => extractFunction(html, n)).join('\n')
    : 'function warnFromWrites(){return [];}function warnText(){return "";}function warnLine(){return "";}');

async function apply({ cols, pk, rows, upd = {}, del = [], types = {}, bin, reply = { ok: true }, session, preview, modeOf, anyway }) {
  const sent = [], toasts = [], sessions = [], shown = [], opened = [], requests = [], runWarn = [];
  const win = { noBackslashEscapes: false };
  const t = { db: 'd', table: 't', cols, pk, rows, binCols: [], exact: true,
              pending: { upd, del: new Set(del), ins: [] }, txSession: session };
  const env = {
    roBlock: () => false, T: () => t, qid: s => '`' + s + '`', log: () => {}, invalidateTableCache: () => {},
    openRun: async () => {}, refreshTabDirty: () => {}, sessOf: tab => tab.txSession, sessFree: async () => {},
    gridBinCols: async () => bin || cols.map(() => false),
    tableColTypes: async () => types, tableNulTextCount: async () => 0, fmtCount: String,
    toast: (m, k) => toasts.push((k === true ? 'ERR ' : '') + m),
    api: async (p, d) => { if (d.sql === 'SELECT @@SESSION.sql_mode') return { ok: true, rows: [[modeOf || '']] }; sent.push(d.sql); sessions.push(d.session); requests.push(d); return typeof reply === 'function' ? reply(d.sql) : reply; },
    viewText: (title, text) => shown.push({ title, text }),
    warnOpen: (title, ws, stmts, again) => opened.push({ title, ws, stmts, again }),
    setRunWarnings: (id, ws) => runWarn.push(ws),
    window: win,
  };
  const keys = Object.keys(env);
  const result = await new Function(...keys, bundle + '\nreturn applyChanges;')(...keys.map(k => env[k]))('x', preview, anyway);
  return { win, sql: sent.join('\n'), stmts: sent.length ? sent[0].split('\n') : [], toasts, sessions, shown, result, pending: t.pending, opened, requests, runWarn };
}
const guardOf = where => 'SELECT 1 FROM (SELECT 1 AS x UNION ALL SELECT 2) nobs_guard WHERE (SELECT COUNT(*) FROM `d`.`t` WHERE ' + where + ') <> 1 INTO @nobs_one_row;';

test('every update and delete is preceded by a guard on the same WHERE', async () => {
  const r = await apply({ cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a'], ['2', 'b']],
                          upd: { '0:1': 'x' }, del: [1], types: { id: 'int', v: 'varchar' } });
  assert.deepEqual(r.stmts, [
    guardOf("`id`='1'"), "UPDATE `d`.`t` SET `v`='x' WHERE `id`='1' LIMIT 1;",
    guardOf("`id`='2'"), "DELETE FROM `d`.`t` WHERE `id`='2' LIMIT 1;",
  ]);
  assert.ok(r.toasts.some(m => m === 'Applied 2 change(s).'), 'the guards are not counted as changes: ' + r.toasts);
});

// Many changes: the checks go 200 to a statement, and so do deletes, which come after the updates -
// two round trips per change made deleting thousands of rows take minutes over a slow link.
test('several changes are checked together, and deletes go together after the updates', async () => {
  const r = await apply({ cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a'], ['2', 'b'], ['3', 'c'], ['4', 'd']],
                          upd: { '0:1': 'x', '1:1': 'y' }, del: [2, 3], types: { id: 'int', v: 'varchar' } });
  const many = ws => 'SELECT 1 FROM (SELECT 1 AS x UNION ALL SELECT 2) nobs_guard WHERE ' + ws.map(w => '(SELECT COUNT(*) FROM `d`.`t` WHERE ' + w + ') <> 1').join(' OR ') + ' INTO @nobs_one_row;';
  assert.deepEqual(r.stmts, [
    many(["`id`='1'", "`id`='2'"]),
    "UPDATE `d`.`t` SET `v`='x' WHERE `id`='1' LIMIT 1;", "UPDATE `d`.`t` SET `v`='y' WHERE `id`='2' LIMIT 1;",
    many(["`id`='3'", "`id`='4'"]),
    "DELETE FROM `d`.`t` WHERE (`id`='3') OR (`id`='4') LIMIT 2;",
  ]);
  assert.ok(r.toasts.includes('Applied 4 change(s).'), 'rows are counted, not statements: ' + r.toasts);
});

test('a FLOAT key is matched by its text', async () => {
  const r = await apply({ cols: ['k', 'v'], pk: ['k'], rows: [['1.1', 'a']], upd: { '0:1': 'x' }, types: { k: 'float' } });
  assert.ok(r.stmts.includes("UPDATE `d`.`t` SET `v`='x' WHERE CAST(`k` AS CHAR)='1.1' LIMIT 1;"), r.sql);
});

test('a TIMESTAMP key is matched by its text, near its time', async () => {
  const r = await apply({ cols: ['ts', 'v'], pk: ['ts'], rows: [['2026-10-25 02:30:00', 'a']], del: [0], types: { ts: 'timestamp' } });
  const where = "(`ts` BETWEEN '2026-10-25 02:30:00' - INTERVAL 3 HOUR AND '2026-10-25 02:30:00' + INTERVAL 3 HOUR AND CAST(`ts` AS CHAR)='2026-10-25 02:30:00')";
  assert.deepEqual(r.stmts, [guardOf(where), 'DELETE FROM `d`.`t` WHERE ' + where + ' LIMIT 1;']);
});

test('binary and composite keys keep their typed form', async () => {
  const r = await apply({ cols: ['a', 'b', 'v'], pk: ['a', 'b'], rows: [['0x01', '0x41', 'v']], upd: { '0:2': 'w' },
                          types: { a: 'varbinary', b: 'varchar' }, bin: [true, false, false] });
  assert.ok(r.stmts.includes("UPDATE `d`.`t` SET `v`='w' WHERE `a`=0x01 AND `b`='0x41' LIMIT 1;"), r.sql);
});

test('a result without the key column saves nothing', async () => {
  const r = await apply({ cols: ['v'], pk: ['id'], rows: [['a']], upd: { '0:0': 'x' }, types: {} });
  assert.equal(r.sql, '');
  assert.ok(r.toasts.some(m => /does not include every key column \(id\)/.test(m)), r.toasts.join());
});

test('a refused guard is explained', async () => {
  const r = await apply({ cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a']], upd: { '0:1': 'x' }, types: {},
                          reply: { ok: false, error: 'ERROR 1172 (42000) at line 3: Result consisted of more than one row' } });
  assert.ok(r.toasts.some(m => m.startsWith('ERR Nothing was saved. A row you changed or deleted no longer matches exactly one row')), r.toasts.join());
});

test('a save in a tab with auto-commit off goes into its transaction', async () => {
  const one = { cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a']], upd: { '0:1': 'b' } };
  assert.deepEqual((await apply({ ...one, session: 'tx_1' })).sessions, ['tx_1']);
  assert.deepEqual((await apply(one)).sessions, [undefined]);
});

// A transaction tab's own session may have had its sql_mode changed since the app connected, and a
// literal written for the wrong mode ends in the wrong place. Apply asks that session.
test('in a transaction the literals follow that session\'s sql_mode', async () => {
  const one = { cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a']], upd: { '0:1': 'x\\y' } };
  assert.match((await apply({ ...one, session: 'tx_1', modeOf: 'STRICT_TRANS_TABLES,NO_BACKSLASH_ESCAPES' })).sql, /SET `v`='x\\y'/);
  assert.match((await apply({ ...one, session: 'tx_1', modeOf: 'STRICT_TRANS_TABLES' })).sql, /SET `v`='x\\\\y'/);
  const r = await apply({ ...one, session: 'tx_1', modeOf: 'NO_BACKSLASH_ESCAPES' });
  assert.equal(r.win.noBackslashEscapes, false, 'and the app-wide setting is back as it was');
});

test('Show SQL shows exactly what Apply would run, and runs nothing', async () => {
  const one = { cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a']], upd: { '0:1': 'b' }, types: { id: 'int', v: 'varchar' } };
  const saved = await apply(one);
  const r = await apply({ ...one, upd: { '0:1': 'b' }, preview: true });
  assert.equal(r.sql, '', 'nothing was sent');
  assert.equal(r.result, false);
  assert.equal(r.shown.length, 1);
  assert.equal(r.shown[0].text, saved.sql, 'the preview is the SQL Apply sends');
  assert.match(r.shown[0].title, /1 change$/);
  assert.deepEqual(Object.keys(r.pending.upd), ['0:1'], 'the edit is still pending');
  assert.equal(saved.result, true, 'and Apply itself says it saved');
});

// A value the server would store otherwise than typed - cut short under a sql_mode without
// STRICT_TRANS_TABLES - stops the save, and the warnings are shown with a way to apply anyway.
const truncated = { statement: 2, level: 'Warning', code: 1265, message: "Data truncated for column 'v' at row 1" };
const warnSkip = hasWarnings ? false : 'this edition does not read the server\'s warnings yet';
const oneEdit = { cols: ['id', 'v'], pk: ['id'], rows: [['1', 'a']], upd: { '0:1': 'a very long value' }, types: { id: 'int', v: 'varchar' } };

test('Apply asks the server to keep nothing when it warns', { skip: warnSkip }, async () => {
  const r = await apply(oneEdit);
  assert.equal(r.requests[0].failOnWarnings, true);
  assert.equal(r.requests[0].transaction, true);
});

test('a save the server warned about is not kept, and the warnings are shown', { skip: warnSkip }, async () => {
  const r = await apply({ ...oneEdit, reply: { ok: false, warned: true, warnings: [truncated] } });
  assert.equal(r.result, false, 'Commit must not go on');
  assert.deepEqual(Object.keys(r.pending.upd), ['0:1'], 'the edit is still pending');
  assert.equal(r.opened.length, 1);
  assert.deepEqual(r.opened[0].ws, [truncated]);
  assert.match(r.opened[0].stmts[1], /^UPDATE `d`\.`t` SET `v`='a very long value'/, 'the list can name the statement');
  assert.ok(!r.toasts.some(m => /^ERR Apply failed/.test(m)), 'it is not reported as a failure: ' + r.toasts);
});

test('Apply anyway saves without the check', { skip: warnSkip }, async () => {
  let n = 0;
  const r = await apply({ ...oneEdit, reply: () => (n++ === 0 ? { ok: false, warned: true, warnings: [truncated] } : { ok: true, warnings: [truncated] }) });
  await r.opened[0].again();
  assert.equal(r.requests.length, 2);
  assert.equal(r.requests[1].failOnWarnings, false);
  assert.ok(r.toasts.some(m => m === 'Applied 1 change(s), with 1 warning.'), r.toasts.join());
  assert.deepEqual(r.runWarn, [[truncated]], 'and what the server said stays beside the status');
});

test('the warning every row guard gives is not one about the edits', { skip: warnSkip }, async () => {
  const guard = { statement: 1, level: 'Warning', code: 1329, message: 'No data - zero rows fetched, selected, or processed' };
  const r = await apply({ ...oneEdit, reply: { ok: true, warnings: [guard] } });
  assert.equal(r.result, true);
  assert.ok(r.toasts.includes('Applied 1 change(s).'), r.toasts.join());
  assert.deepEqual(r.runWarn, [], 'nothing to show');
});

// Copied cells: the block keeps the shape they were picked in, and a paste puts them back in it.
const CELL_NAMES = ['pickedCellsGrid', 'cellClipCount', 'pasteCellsFrom', 'pasteCellsInto', 'setUpdMany'];
const cellBundle = CELL_NAMES.map(n => extractFunction(html, n)).join('\n');
function cells({ rows, view, sel, clip, gen = [], notNull = [] }) {
  const t = { cols: ['a', 'b', 'c'], rows, cellSel: new Set(sel || []), pending: { upd: {}, del: new Set(), ins: [] } };
  const toasts = [], win = { _cellClipboard: clip };
  const env = { T: () => t, viewIndices: () => view || rows.map((r, i) => i), isGenCol: (id, c) => gen.includes(c), canNull: (id, c) => !notNull.includes(c),
    renderGrid: () => {}, toast: (m, k) => toasts.push((k === true ? 'ERR ' : '') + m), window: win };
  const keys = Object.keys(env);
  const f = new Function(...keys, cellBundle + '\nreturn {' + CELL_NAMES.join(',') + '};')(...keys.map(k => env[k]));
  return { t, f, win, toasts };
}
const rows3 = [['a0', 'b0', 'c0'], ['a1', null, 'c1'], ['a2', 'b2', 'c2']];

test('cells picked apart keep their places, with gaps between', () => {
  const { f } = cells({ rows: rows3, sel: ['0:0', '2:2'] });
  assert.deepEqual(f.pickedCellsGrid('x'), [['a0', undefined, undefined], [undefined, undefined, undefined], [undefined, undefined, 'c2']]);
});

test('the block follows the rows as shown, not their numbers', () => {
  const { f } = cells({ rows: rows3, view: [2, 1, 0], sel: ['2:0', '0:0'] });
  assert.deepEqual(f.pickedCellsGrid('x'), [['a2'], [undefined], ['a0']]);
});

test('a NULL is copied as NULL, and only picked cells are counted', () => {
  const { f, win } = cells({ rows: rows3, sel: ['1:1', '0:0'] });
  win._cellClipboard = f.pickedCellsGrid('x');
  assert.deepEqual(win._cellClipboard, [['a0', undefined], [undefined, null]]);
  assert.equal(f.cellClipCount(), 2);
});

test('a block lands from the cell it is pasted on, gaps leaving cells as they were', () => {
  const { t, f } = cells({ rows: rows3, clip: [['x', undefined], [undefined, 'y']] });
  f.pasteCellsFrom('x', 1, 1);
  assert.deepEqual(t.pending.upd, { '1:1': 'x', '2:2': 'y' });
});

test('what falls outside the grid is left out, and said so', () => {
  const { t, f, toasts } = cells({ rows: rows3, clip: [['x', 'y']] });
  f.pasteCellsFrom('x', 2, 2);
  assert.deepEqual(t.pending.upd, { '2:2': 'x' });
  assert.ok(toasts.some(m => /1 copied value falls outside the grid/.test(m)), toasts.join());
});

test('into picked cells: one value fills them all, as many values go in reading order', () => {
  const one = cells({ rows: rows3, clip: [['z']] });
  one.f.pasteCellsInto('x', ['2:0', '0:0']);
  assert.deepEqual(one.t.pending.upd, { '0:0': 'z', '2:0': 'z' });
  const many = cells({ rows: rows3, view: [0, 1, 2], clip: [['p', undefined], [undefined, 'q']] });
  many.f.pasteCellsInto('x', ['2:1', '0:2']);
  assert.deepEqual(many.t.pending.upd, { '0:2': 'p', '2:1': 'q' });
});

test('a paste leaves generated columns alone, and NULL out of a column that cannot hold it', () => {
  const { t, f, toasts } = cells({ rows: rows3, clip: [[null, null, null]], gen: ['b'], notNull: ['c'] });
  f.pasteCellsFrom('x', 0, 0);
  assert.deepEqual(t.pending.upd, { '0:0': null });
  assert.ok(toasts.some(m => /generated column/.test(m)) && toasts.some(m => /cannot hold NULL/.test(m)), toasts.join());
});
