// What the editor checks before and after it runs something (ui/index.html): an UPDATE or DELETE
// without a WHERE, which statements may be measured, and how MySQL's EXPLAIN ANALYZE and MariaDB's
// ANALYZE FORMAT=JSON are read.
//
// As in the other tests here, the functions are lifted out of the page and run, rather than
// copied, so a regression in the page is a failure here.
//
// Run: node --test tests/ui/     (or npm test)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
// NOBS_UI_SOURCE lets the PowerShell edition run this same file against NOBSSQL.ps1, which carries
// the same page inline.
const html = readFileSync(process.env.NOBS_UI_SOURCE || join(root, 'ui', 'index.html'), 'utf8');

function extractFunction(src, name) {
  const start = src.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `function ${name} not found - was it renamed?`);
  let i = src.indexOf('{', start), depth = 0;
  for (let j = i; j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`unbalanced braces while extracting ${name}`);
}
function extractConst(src, name) {
  const start = src.indexOf(`const ${name}=`);
  assert.notEqual(start, -1, `const ${name} not found - was it renamed?`);
  return src.slice(start, src.indexOf(';', src.indexOf(name === 'PLAN_SKIP' ? '])' : '}', start)) + 1);
}

const helpers = `const esc=s=>String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
const clip=(s,n)=>String(s).slice(0,n);const fmtCount=n=>String(Math.round(+n||0));`;
const lib = new Function(helpers + '\n' + ['PLAN_ACCESS', 'PLAN_STEP', 'PLAN_SKIP'].map(n => extractConst(html, n)).join('\n') + '\n' +
  ['sqlHead', 'sqlBlankStringsAndComments', 'unfilteredWrites', 'planReadsOnly', 'planMiss', 'planMs', 'planNum', 'planAccess', 'planTable',
   'planItems', 'planNode', 'planHtml', 'planAnalyzeSteps', 'planAnalyzeKind', 'planAnalyzeHtml', 'fmtPsTime'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {unfilteredWrites, planReadsOnly, planMiss, planAnalyzeSteps, planAnalyzeHtml, planHtml, fmtPsTime};')();

const w = sql => lib.unfilteredWrites([sql]);

test('an UPDATE or DELETE without a WHERE is caught, with its table', () => {
  assert.deepEqual(w('UPDATE orders SET paid = 1'), [{ verb: 'UPDATE', table: 'orders' }]);
  assert.deepEqual(w('delete from shop.log'), [{ verb: 'DELETE', table: 'shop.log' }]);
  assert.deepEqual(w('DELETE LOW_PRIORITY QUICK IGNORE FROM `my table`'), [{ verb: 'DELETE', table: '`my table`' }]);
  assert.deepEqual(w('UPDATE IGNORE t SET a = 1 LIMIT 10'), [{ verb: 'UPDATE', table: 't' }], 'a LIMIT is not a WHERE');
  assert.deepEqual(w('-- tidy up\n/* all of it */ DELETE FROM t'), [{ verb: 'DELETE', table: 't' }], 'comments in front are read past');
  assert.deepEqual(w('WITH x AS (SELECT 1 FROM a WHERE b) UPDATE t SET c = 1'), [{ verb: 'UPDATE', table: 't' }], 'a WITH is read past');
});

test('a WHERE of its own, a JOIN, or no write at all is not caught', () => {
  for (const sql of [
    'UPDATE t SET a = 1 WHERE id = 2',
    'delete from t where x in (select 1)',
    'UPDATE t JOIN u ON u.id = t.uid SET t.a = u.a',
    'DELETE t FROM t INNER JOIN u ON u.id = t.uid',
    'SELECT * FROM t',
    'SELECT * FROM t FOR UPDATE',
    'CREATE PROCEDURE p() BEGIN DELETE FROM t; END',
    'EXPLAIN UPDATE t SET a = 1',
    'INSERT INTO t VALUES (1)',
  ]) assert.deepEqual(w(sql), [], sql);
});

test('a WHERE only in a subquery, a string, a comment or a name does not count', () => {
  assert.equal(w('UPDATE t SET a = (SELECT MAX(b) FROM u WHERE u.id = 1)').length, 1);
  assert.equal(w("UPDATE t SET note = 'where it goes'").length, 1);
  assert.equal(w('UPDATE t SET a = 1 -- WHERE id = 2').length, 1);
  assert.equal(w('UPDATE t SET a = 1 /* WHERE id = 2 */').length, 1);
  assert.equal(w('UPDATE `where` SET a = 1').length, 1);
});

test('every statement of a script is looked at', () => {
  const got = lib.unfilteredWrites(['UPDATE a SET x = 1 WHERE id = 1', 'DELETE FROM b', 'UPDATE c SET y = 2']);
  assert.deepEqual(got.map(x => x.verb + ' ' + x.table), ['DELETE b', 'UPDATE c']);
});

test('only a query that reads is measured', () => {
  for (const sql of ['SELECT * FROM t', 'with x as (select 1) select * from x', 'TABLE t', 'VALUES ROW(1)', "SELECT 'update' FROM t"])
    assert.equal(lib.planReadsOnly(sql), true, sql);
  for (const sql of ['UPDATE t SET a = 1', 'DELETE FROM t', 'SELECT * FROM t FOR UPDATE', 'SELECT * FROM t LOCK IN SHARE MODE',
    'SELECT * INTO OUTFILE \'/tmp/x\' FROM t', 'WITH x AS (SELECT 1) DELETE FROM t', 'INSERT INTO t SELECT 1'])
    assert.equal(lib.planReadsOnly(sql), false, sql);
});

test('an estimate ten times off is a miss, either way', () => {
  assert.equal(lib.planMiss(10, 100), true);
  assert.equal(lib.planMiss(1000, 90), true);
  assert.equal(lib.planMiss(10, 50), false);
  assert.equal(lib.planMiss(0, 5), false, 'no rows expected and a few read is not worth a warning');
  assert.equal(lib.planMiss(null, 5), false);
});

// Captured from MySQL 8.4.
const MYSQL_ANALYZE = `-> Nested loop inner join  (cost=4.95 rows=9) (actual time=0.153..0.2 rows=9 loops=1)
    -> Filter: (t1.a is not null)  (cost=1.15 rows=9) (actual time=0.067..0.086 rows=9 loops=1)
        -> Table scan on t1  (cost=1.15 rows=9) (actual time=0.065..0.082 rows=9 loops=1)
    -> Index lookup on t2 using a (a=t1.a)  (cost=0.26 rows=1) (actual time=0.01..0.012 rows=40 loops=9)
    -> Select #2 (subquery in condition; run only once)
        -> Table scan on t3  (cost=0.35 rows=1) (never executed)`;

test('MySQL EXPLAIN ANALYZE is read into a tree by its indentation', () => {
  const s = lib.planAnalyzeSteps(MYSQL_ANALYZE);
  assert.equal(s.length, 1);
  assert.equal(s[0].label, 'Nested loop inner join');
  assert.deepEqual(s[0].kids.map(k => k.label), ['Filter: (t1.a is not null)', 'Index lookup on t2 using a (a=t1.a)', 'Select #2 (subquery in condition; run only once)']);
  const scan = s[0].kids[0].kids[0];
  assert.deepEqual([scan.label, scan.est, scan.rows, scan.loops], ['Table scan on t1', 9, 9, 1]);
  const look = s[0].kids[1];
  assert.equal(look.ms.toFixed(3), (0.012 * 9).toFixed(3), 'the time of a step is its time per loop times its loops');
  assert.equal(s[0].kids[2].kids[0].never, true);
});

test('the measured MySQL plan names full scans and estimates that were off', () => {
  const h = lib.planAnalyzeHtml(MYSQL_ANALYZE);
  assert.match(h, /Took 0\.20 ms/);
  assert.match(h, /1 table was read in full: t1/, 'a scan that never ran is not counted');
  assert.match(h, /1 step expected ten times too many or too few rows/);
  assert.match(h, /!! 40 rows × 9 loops \(expected 1\)/);
  assert.match(h, /never run/);
  assert.match(h, /pcard bad">Table scan on t1/);
});

test('MariaDB ANALYZE FORMAT=JSON shows what was read beside what was expected', () => {
  const plan = { query_block: { select_id: 1, r_loops: 1, r_total_time_ms: 3.456, table: {
    table_name: 't', access_type: 'ALL', r_loops: 1, rows: 10, r_rows: 1000, filtered: 100, r_filtered: 5, r_table_time_ms: 2.5, r_other_time_ms: 0.25 } } };
  const h = lib.planHtml(JSON.stringify(plan));
  assert.match(h, /took 3\.5 ms/);
  assert.match(h, /!! read 1000 rows \(expected 10\)/);
  assert.match(h, /5% kept/);
  assert.match(h, /2\.8 ms/);
  assert.doesNotMatch(lib.planHtml(JSON.stringify({ query_block: { table: { table_name: 't', access_type: 'ALL', rows: 10 } } })), /read \d/, 'an estimate says nothing about what was read');
});

test('performance_schema time reads as a duration', () => {
  assert.equal(lib.fmtPsTime(5e8), '0.50 ms');
  assert.equal(lib.fmtPsTime(2.5e11), '250 ms');
  assert.equal(lib.fmtPsTime(3.21e12), '3.21 s');
  assert.equal(lib.fmtPsTime(125e12), '2m 5s');
  assert.equal(lib.fmtPsTime(7200e12), '2h 0m');
  assert.equal(lib.fmtPsTime(3 * 86400e12), '3d 0h');
});
