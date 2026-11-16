// Which restored tabs run when you connect, and when.
//
// A table tab ran whatever its editor held when its connection's tabs came back - a query of your
// own written over the table's SELECT included, against whichever server you had just connected
// to - and every table tab ran at once. Now only a table tab still holding the table's own query
// runs again, and only once it is shown: on connect just the tab in front reads. The rest come back
// not run.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const html = readFileSync(process.env.NOBS_UI_SOURCE ||
  join(dirname(fileURLToPath(import.meta.url)), '../../ui/index.html'), 'utf8').replace(/\r\n/g, '\n');

function extractFunction(src, name) {
  const start = src.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `function ${name} not found - was it renamed?`);
  let depth = 0;
  for (let j = src.indexOf('{', start); j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`unbalanced braces while extracting ${name}`);
}

// Restores `saved` (what saveSession wrote). Per tab: whether it has run, whether it waits to be
// shown, what it holds, and show() to switch to it. The last tab opened is the one in front, as in
// the app; activate() runs a waiting tab the way the app's does.
function restore(saved) {
  const opened = [], status = {}, ran = [], store = { 'session:k': JSON.stringify(saved) };
  const env = {
    front: null,
    tabs: [],
    localStorage: { getItem: k => (k in store ? store[k] : null), setItem: (k, v) => { store[k] = v; } },
    qid: s => '`' + s + '`',
    openTab: (title, sql, db, run, table, ddl) => {
      const id = 't' + opened.length; opened.push({ id, title, sql, db, table, ddl, genSql: sql, sqlEdited: false });
      env.front = id; if (run) ran.push(id); return id;
    },
    activate: id => { env.front = id; const t = opened.find(x => x.id === id); if (t.runOnShow) { t.runOnShow = false; ran.push(id); } },
    T: id => opened.find(t => t.id === id),
    markEdited: id => { const t = opened.find(x => x.id === id); t.sqlEdited = t.sql !== t.genSql; },
    updateFilterBar: () => {},
    refreshTabDirty: () => {},
    $: id => (id.startsWith('st_') ? (status[id.slice(3)] = status[id.slice(3)] || {}) : null),
  };
  const src = ['restoredTableSql', 'restoreSessionFor'].map(n => extractFunction(html, n)).join('\n');
  // activeTab is the app's global for the tab in front; here it follows env.front.
  Object.defineProperty(env, 'activeTab', { get: () => env.front });
  new Function('env', 'with (env) { return (function () {\n' + src + '\nreturn restoreSessionFor; })(); }')(env)('k');
  return opened.map(t => ({
    title: t.title, waits: () => !!t.runOnShow, edited: t.sqlEdited, filters: t.filterClauses,
    status: (status[t.id] || {}).textContent, ran: () => ran.includes(t.id), show: () => env.activate(t.id),
  }));
}

test('on connect only the table tab in front reads its table; the others read when shown', () => {
  const r = restore([
    { title: 'user', sql: 'SELECT * FROM `mysql`.`user`;', db: 'mysql', table: 'user' },
    { title: 'slow_log', sql: 'SELECT * FROM `mysql`.`slow_log`;', db: 'mysql', table: 'slow_log' },
    { title: 'txn_composite', sql: 'SELECT * FROM `nobs_test`.`txn_composite`;', db: 'nobs_test', table: 'txn_composite' },
  ]);
  assert.deepEqual(r.map(t => t.ran()), [false, false, true], 'the last one is in front, and only it ran');
  assert.deepEqual(r.map(t => t.waits()), [true, true, false]);
  assert.match(r[0].status, /when you open this tab/);
  r[0].show();
  assert.deepEqual(r.map(t => t.ran()), [true, false, true], 'shown, it reads');
  r[2].show(); r[0].show();
  assert.equal(r[0].waits(), false, 'and only the first time');
});

test('a table tab holding a query of yours does not run, shown or not', () => {
  const r = restore([
    { title: 'ro_canary', sql: 'SELECT i.id FROM other_db.identity i WHERE i.p = 0;', db: 'nobs_test', table: 'ro_canary' },
    { title: 'Query', sql: 'DELETE FROM t WHERE id = 1;', db: 'nobs_test', table: null },
    { title: 'a', sql: 'SELECT * FROM `d`.`a`;', db: 'd', table: 'a' },
  ]);
  r[0].show(); r[1].show();
  assert.deepEqual(r.map(t => t.ran()), [false, false, true], JSON.stringify(r));
  assert.equal(r[0].edited, true, 'the replaced one counts as edited, so it runs against the schema picked');
  assert.match(r[0].status, /not yet run/);
  assert.match(r[1].status, /not yet run/);
});

test('a table tab with quick filters runs again with them', () => {
  const [t] = restore([{ title: 'a', sql: 'SELECT * FROM `d`.`a` WHERE `x` = 1 AND `y` IS NULL;', db: 'd', table: 'a', filters: ['`x` = 1', '`y` IS NULL'] }]);
  assert.equal(t.ran(), true);
  assert.deepEqual(t.filters, ['`x` = 1', '`y` IS NULL']);
});

test('a table query with something added to it is yours, and is not run', () => {
  for (const sql of ['SELECT * FROM `d`.`a` LIMIT 5;', 'SELECT * FROM `d`.`a`; DROP TABLE x;', 'SELECT * FROM `d`.`b`;', 'select * from `d`.`a` where 1;'])
    assert.equal(restore([{ title: 'a', sql, db: 'd', table: 'a' }])[0].ran(), false, sql);
  assert.equal(restore([{ title: 'a', sql: '  SELECT * FROM `d`.`a`;\n', db: 'd', table: 'a' }])[0].ran(), true, 'spaces around it do not count');
});
