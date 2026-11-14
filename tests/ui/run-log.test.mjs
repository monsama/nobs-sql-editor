// What the output window says about a run (ui/index.html): a statement on one line, shortened only
// past LOG_SQL_MAX characters, and a shortened line carrying the whole statement behind an expander.
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
  return src.slice(start, src.indexOf(';', start) + 1);
}

// Just enough of a DOM for log(): elements with text, a class list, children and a click handler.
function el() {
  const cls = new Set();
  return {
    children: [], textContent: '', title: '', onclick: null,
    set className(v) { cls.clear(); String(v).split(/\s+/).filter(Boolean).forEach(c => cls.add(c)); },
    get className() { return [...cls].join(' '); },
    classList: { add: c => cls.add(c), contains: c => cls.has(c), toggle: c => (cls.has(c) ? (cls.delete(c), false) : (cls.add(c), true)) },
    append(...k) { this.children.push(...k); }, appendChild(k) { this.children.push(k); },
  };
}

const build = new Function(
  'T', 'runClock', 'logKind', 'sqlNoSecrets', 'logJump', '$', 'document', 'stmtOutcome',
  [extractConst(html, 'LOG_SQL_MAX'), ...['sqlOneLine', 'sqlWhole', 'runLog', 'log', 'stmtLines'].map(n => extractFunction(html, n))].join('\n')
  + '\nreturn {LOG_SQL_MAX, LOG_SQL_FULL_MAX, sqlOneLine, sqlWhole, runLog, stmtLines};');
const logBox = el();
const page = build(() => ({ title: 'Query 1' }), () => '12:00:00', () => '', s => String(s), () => {}, () => logBox,
  { createElement: el }, st => st.affected + ' rows affected');
const lastLine = () => logBox.children[logBox.children.length - 1];

test('a statement is shortened only past LOG_SQL_MAX characters, on one line', () => {
  assert.ok(page.LOG_SQL_MAX >= 200, 'not cut off after a few words: ' + page.LOG_SQL_MAX);
  const fits = 'SELECT ' + 'a,'.repeat(Math.floor((page.LOG_SQL_MAX - 20) / 2)) + 'b FROM t';
  assert.ok(fits.length <= page.LOG_SQL_MAX);
  assert.equal(page.sqlOneLine(fits), fits);
  assert.equal(page.sqlOneLine('SELECT 1\n  FROM t'), 'SELECT 1 FROM t');
  const long = 'SELECT ' + 'x'.repeat(page.LOG_SQL_MAX);
  assert.equal(page.sqlOneLine(long), long.slice(0, page.LOG_SQL_MAX) + '...');
  assert.equal(page.sqlWhole('  SELECT 1\n  FROM t \n'), 'SELECT 1\n  FROM t', 'kept as written');
  assert.equal(page.sqlWhole('x'.repeat(page.LOG_SQL_FULL_MAX + 5)).length, page.LOG_SQL_FULL_MAX + 3, 'within reason');
});

test('a short statement is a plain line, with no expander', () => {
  page.runLog('1', f => f('SELECT 1') + ' -> 1 row');
  const d = lastLine();
  assert.equal(d.textContent, '[12:00:00] Query 1: SELECT 1 -> 1 row');
  assert.equal(d.children.length, 0);
  page.runLog('1', 'APPLY OK');
  assert.equal(lastLine().textContent, '[12:00:00] Query 1: APPLY OK', 'plain text still works');
});

test('a long statement gets an expander that shows all of it and back', () => {
  const sql = 'SELECT id,\n       ' + 'name'.repeat(100) + '\n  FROM t';
  page.runLog('1', f => f(sql) + ' -> 1 row');
  const d = lastLine(), [b, x] = d.children;
  assert.equal(b.className, 'lnx');
  assert.equal(x.textContent, '[12:00:00] Query 1: ' + page.sqlOneLine(sql) + ' -> 1 row');
  let stopped = false;
  b.onclick({ stopPropagation: () => { stopped = true; } });
  assert.ok(stopped, 'opening it is not a click on the line, which jumps to the statement');
  assert.equal(x.textContent, '[12:00:00] Query 1: ' + sql + ' -> 1 row');
  assert.ok(d.classList.contains('open'));
  b.onclick({ stopPropagation() {} });
  assert.equal(x.textContent, '[12:00:00] Query 1: ' + page.sqlOneLine(sql) + ' -> 1 row');
});

test("a script's statements are shortened on the line, and whole in its expander", () => {
  const sts = [{ index: 1, sql: 'UPDATE t SET v = ' + "'" + 'v'.repeat(400) + "'", affected: 3 }, { index: 2, sql: 'DELETE FROM t', affected: 1 }];
  page.runLog('1', f => 'SCRIPT OK\n' + page.stmtLines(sts, 2, f).join('\n'));
  const [, x] = lastLine().children;
  assert.match(x.textContent, /\n {2}1\/2\. UPDATE t SET v = 'v+\.\.\. -> 3 rows affected\n {2}2\/2\. DELETE FROM t -> 1 rows affected$/);
  lastLine().children[0].onclick({ stopPropagation() {} });
  assert.ok(x.textContent.includes(sts[0].sql + ' -> 3 rows affected'));
});
