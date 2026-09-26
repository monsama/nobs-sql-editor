// How a script is cut into statements (ui/index.html), and which one Run and Explain take at the
// cursor when nothing is selected. Cut in the wrong place, Ctrl+Enter sends the wrong statement - or
// a DELETE along with the SELECT the cursor was on.
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

// window: whether the server runs with NO_BACKSLASH_ESCAPES
const window = { noBackslashEscapes: false };
const constLine = name => { const i = html.indexOf(`const ${name}=`); assert.notEqual(i, -1, `const ${name} not found`); return html.slice(i, html.indexOf('\n', i)); };
const { splitStmts, stmtAtCursor } = new Function('window', constLine('TAIL_NOTE') + '\n' +
  ['sqlHead', 'isCommentOnly', 'splitStmts', 'stmtAtCursor'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {splitStmts, stmtAtCursor};')(window);

// the statement at the "|" in the text
const at = sql => stmtAtCursor(sql.replace('|', ''), sql.indexOf('|'));

test('statements are cut at each ;', () => {
  assert.deepEqual(splitStmts('select 1; select 2;\nselect 3'), ['select 1', 'select 2', 'select 3']);
  assert.deepEqual(splitStmts(' ;; select 1 ;  '), ['select 1'], 'empty ones are no statements');
  assert.deepEqual(splitStmts(''), []);
});

test('a ; in a string or a quoted name does not cut', () => {
  assert.deepEqual(splitStmts("select 'a;b'; select \"c;d\"; select `e;f` from t"), ["select 'a;b'", 'select "c;d"', 'select `e;f` from t']);
  assert.deepEqual(splitStmts("select 'it''s; fine'; select 2"), ["select 'it''s; fine'", 'select 2']);
  assert.deepEqual(splitStmts("select 'a\\';b'; select 2"), ["select 'a\\';b'", 'select 2'], 'an escaped quote does not end the string');
});

test('with NO_BACKSLASH_ESCAPES a backslash is only a backslash', () => {
  window.noBackslashEscapes = true;
  try { assert.deepEqual(splitStmts("select 'C:\\'; delete from t"), ["select 'C:\\'", 'delete from t']); }
  finally { window.noBackslashEscapes = false; }
  assert.deepEqual(splitStmts("select 'C:\\'; delete from t"), ["select 'C:\\'; delete from t"], 'with them, the quote is escaped and the string runs on');
});

test('a ; or a quote in a comment does not cut', () => {
  assert.deepEqual(splitStmts('select 1 -- one; two\n; select 2'), ['select 1 -- one; two', 'select 2']);
  assert.deepEqual(splitStmts("# Don't run the next one\nselect 1; delete from t"), ["# Don't run the next one\nselect 1", 'delete from t']);
  assert.deepEqual(splitStmts('select /* a; b */ 1; select 2'), ['select /* a; b */ 1', 'select 2']);
});

test('"--" is a comment only before a space, as the server reads it', () => {
  assert.deepEqual(splitStmts('select 1--1; delete from t'), ['select 1--1', 'delete from t']);
  assert.deepEqual(splitStmts('select 1 --\nfrom dual; select 2'), ['select 1 --\nfrom dual', 'select 2'], '-- at the end of a line');
});

test('DELIMITER changes what cuts, for a routine with ; inside', () => {
  const s = 'DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\nDELIMITER ;\nCALL p();';
  assert.deepEqual(splitStmts(s), ['CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END', 'CALL p()']);
  assert.deepEqual(splitStmts('delimiter $$\nselect 1$$ select 2$$'), ['select 1', 'select 2'], 'in any case, any delimiter');
});

test('DELIMITER is a command only at the start of a line', () => {
  assert.deepEqual(splitStmts('SELECT csv_delimiter FROM t; SELECT 2'), ['SELECT csv_delimiter FROM t', 'SELECT 2']);
  assert.deepEqual(splitStmts("SELECT 1 delimiter ;"), ['SELECT 1 delimiter']);
});

test('Windows line endings cut the same', () => {
  assert.deepEqual(splitStmts('select 1;\r\nselect 2;\r\n'), ['select 1', 'select 2']);
});

test('with positions: where each statement stands in the text', () => {
  const s = 'select 1;\n  select 2;';
  const p = splitStmts(s, true);
  assert.deepEqual(p.map(x => x.text), ['select 1', 'select 2']);
  assert.equal(s.slice(p[0].start, p[0].end), 'select 1');
  assert.equal(s.slice(p[1].start, p[1].end).trim(), 'select 2');
  assert.equal(p[1].end, s.length - 1, 'up to its delimiter');
});

test('the cursor in a statement takes that statement', () => {
  assert.equal(at('select 1;\nsel|ect 2;\nselect 3;'), 'select 2');
  assert.equal(at('|select 1;\nselect 2;'), 'select 1');
  assert.equal(at('select 1|;\nselect 2;'), 'select 1', 'just before its ;');
});

test('the cursor just after a ; takes the statement it ends, not the next one', () => {
  assert.equal(at('SELECT * FROM t WHERE id=5;|\nDELETE FROM t;'), 'SELECT * FROM t WHERE id=5');
  assert.equal(at('select 1;   |\nselect 2;'), 'select 1', 'trailing spaces too');
  assert.equal(at('select 1; -- done|\nselect 2;'), 'select 1', 'and a comment after it');
  assert.equal(at('select 1;| select 2;'), 'select 1', 'even with the next on the same line');
  assert.equal(at('select 1; |select 2;'), 'select 2', 'but at the start of the next, that one');
});

test('on a line of its own, the next statement', () => {
  assert.equal(at('select 1;\n|\nselect 2;'), 'select 2');
  assert.equal(at('select 1;\n-- the second|\nselect 2;'), '-- the second\nselect 2', 'a comment belongs to what follows it');
});

test('past the last statement, the last one', () => {
  assert.equal(at('select 1;\nselect 2;\n\n|'), 'select 2');
});

test('nothing to run: only comments, or nothing', () => {
  assert.equal(at('-- just a note|'), null);
  assert.equal(at('|'), null);
});

test('a routine between DELIMITER lines is one statement at the cursor', () => {
  assert.equal(at('DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1;| SELECT 2; END//\nDELIMITER ;'), 'CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END');
});

test('Windows line endings: the cursor counts as the editor counts', () => {
  assert.equal(stmtAtCursor('select 1;\r\nselect 2;', 'select 1;\nsel'.length), 'select 2');
});
