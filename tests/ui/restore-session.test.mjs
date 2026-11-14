// Which restored tabs run on their own when you connect.
//
// A table tab ran whatever its editor held when its connection's tabs came back - a query of your
// own written over the table's SELECT included, against whichever server you had just connected
// to. Only a table tab still holding the table's own query runs again now; the rest come back not
// run.

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

// Restores `saved` (what saveSession wrote) and says, per tab, whether it ran and what it holds.
function restore(saved) {
  const opened = [], status = {}, store = { 'session:k': JSON.stringify(saved) };
  const env = {
    tabs: [],
    localStorage: { getItem: k => (k in store ? store[k] : null), setItem: (k, v) => { store[k] = v; } },
    qid: s => '`' + s + '`',
    openTab: (title, sql, db, run, table, ddl) => { const id = 't' + opened.length; opened.push({ id, title, sql, db, run, table, ddl, genSql: sql, sqlEdited: false }); return id; },
    T: id => opened.find(t => t.id === id),
    markEdited: id => { const t = opened.find(x => x.id === id); t.sqlEdited = t.sql !== t.genSql; },
    updateFilterBar: () => {},
    refreshTabDirty: () => {},
    $: id => (id.startsWith('st_') ? (status[id.slice(3)] = status[id.slice(3)] || {}) : null),
  };
  const keys = Object.keys(env);
  new Function(...keys, ['restoredTableSql', 'restoreSessionFor'].map(n => extractFunction(html, n)).join('\n') + '\nreturn restoreSessionFor;')(...keys.map(k => env[k]))('k');
  return opened.map(t => ({ title: t.title, run: t.run, edited: t.sqlEdited, filters: t.filterClauses, status: (status[t.id] || {}).textContent }));
}

test('a table tab holding its own query runs again; one holding a query of yours does not', () => {
  const r = restore([
    { title: 'ro_canary', sql: 'SELECT * FROM `nobs_test`.`ro_canary`;', db: 'nobs_test', table: 'ro_canary' },
    { title: 'ro_canary (2)', sql: 'SELECT i.id FROM other_db.identity i WHERE i.p = 0;', db: 'nobs_test', table: 'ro_canary' },
    { title: 'Query', sql: 'DELETE FROM t WHERE id = 1;', db: 'nobs_test', table: null },
  ]);
  assert.deepEqual(r.map(t => t.run), [true, false, false], JSON.stringify(r));
  assert.equal(r[1].edited, true, 'the replaced one counts as edited, so it runs against the schema picked');
  assert.match(r[1].status, /not yet run/);
  assert.match(r[2].status, /not yet run/);
  assert.equal(r[0].status, undefined, 'the table tab that ran says nothing about not running');
});

test('a table tab with quick filters runs again with them', () => {
  const [t] = restore([{ title: 'a', sql: 'SELECT * FROM `d`.`a` WHERE `x` = 1 AND `y` IS NULL;', db: 'd', table: 'a', filters: ['`x` = 1', '`y` IS NULL'] }]);
  assert.equal(t.run, true);
  assert.deepEqual(t.filters, ['`x` = 1', '`y` IS NULL']);
});

test('a table query with something added to it is yours, and is not run', () => {
  for (const sql of ['SELECT * FROM `d`.`a` LIMIT 5;', 'SELECT * FROM `d`.`a`; DROP TABLE x;', 'SELECT * FROM `d`.`b`;', 'select * from `d`.`a` where 1;'])
    assert.equal(restore([{ title: 'a', sql, db: 'd', table: 'a' }])[0].run, false, sql);
  assert.equal(restore([{ title: 'a', sql: '  SELECT * FROM `d`.`a`;\n', db: 'd', table: 'a' }])[0].run, true, 'spaces around it do not count');
});
