// Text the app reads or writes on the user's behalf (ui/index.html): the SQL formatter, which must
// never change what a statement means; whether a run only reads, for the transaction bar; the
// members of an ENUM or SET; the CSV and Markdown exports; line endings kept on an edited value;
// and a binary value told apart as an image or as text.
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
  let depth = 0;
  for (let j = start; j < src.length; j++) {
    const c = src[j];
    if ('{[('.includes(c)) depth++;
    else if ('}])'.includes(c)) depth--;
    else if (c === ';' && depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`no end to const ${name}`);
}

// the export's NULL marker comes from a box on the page; here, its default
const helpers = `const $=()=>null;`;
const lib = new Function(helpers + '\n' + extractConst(html, 'IMAGE_SIGS') + '\n' +
  ['formatSql', 'txReadsOnly', 'txReadsOnlyAs', 'parseQuotedOptionList', 'csvNullMarker', 'bCSV', 'bMD', 'keepLineEnds', 'detectImageMime', 'hexToBytes', 'hexIsUtf8'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {formatSql, txReadsOnly, parseQuotedOptionList, bCSV, bMD, keepLineEnds, detectImageMime, hexIsUtf8};')();

// The formatter only moves whitespace: with every run of whitespace outside strings and comments
// taken out, the statement before and after is the same text.
const squeeze = sql => sql.replace(/('(?:[^'\\]|\\[\s\S]|'')*'|"(?:[^"\\]|\\[\s\S]|"")*"|`(?:[^`]|``)*`|\/\*[\s\S]*?\*\/|--[ \t][^\n]*|#[^\n]*)|\s+/g, (m, keep) => keep || '');

const CORPUS = [
  "select a, b from t where x = 1 and y in (1, 2, 3) order by a desc limit 10",
  "SELECT o.id, c.name FROM orders o LEFT JOIN customers c ON c.id = o.customer_id WHERE o.total > 10 GROUP BY c.name HAVING COUNT(*) > 1",
  "insert into t (a, b) values (1, 'x, y'), (2, 'it''s')",
  "update t set a = 'select from where', b = b + 1 where id = 3",
  "delete from t where note like '%;%' -- the ; in a string\nand id > 0",
  "select `from`, `select` from `my table` where `where` = 'x' # a comment\n",
  "select 0x1F, 0b101, 1.5e3, .5, -3, @v, @@session.sql_mode, a->>'$.k' from t",
  "select * from a union all select * from b union select * from c",
  "select /* inline, with ; and 'quotes' */ 1",
  "select 'multi\nline', \"double \\\" quoted\" from dual",
  "with x as (select 1 as n) select n from x",
];

test('formatting only moves whitespace: nothing is added to or taken from the statement', () => {
  for (const sql of CORPUS) assert.equal(squeeze(lib.formatSql(sql)), squeeze(sql), sql);
});

test('formatting twice gives what formatting once gave', () => {
  for (const sql of CORPUS) { const once = lib.formatSql(sql); assert.equal(lib.formatSql(once), once, sql); }
});

test('each clause starts a line; the joins and compound keywords stay together', () => {
  assert.equal(lib.formatSql('select a from t left join u on u.id=t.id where a=1 group by a order by a limit 5'),
    'select a\nfrom t\nleft join u\non u.id = t.id\nwhere a = 1\ngroup by a\norder by a\nlimit 5');
});

test('a subquery in brackets is left on its line', () => {
  assert.equal(lib.formatSql('select a from t where id in (select id from u where b=1)'), 'select a\nfrom t\nwhere id in (select id from u where b = 1)');
});

test('what follows a line comment goes on a new line, so it is not commented out', () => {
  const f = lib.formatSql('select a -- the a\nfrom t');
  assert.match(f, /-- the a\nfrom t/);
});

test('keywords inside strings and quoted names are not clauses', () => {
  const f = lib.formatSql("select 'from where' as `select` from t");
  assert.equal(f.split('\n').length, 2, f);
  assert.ok(f.includes("'from where'") && f.includes('`select`'));
});

test('a name after a dot is not a keyword', () => {
  assert.equal(lib.formatSql('select t.from, t.limit from t'), 'select t.from, t.limit\nfrom t');
});

test('a run that only reads', () => {
  for (const sql of ['SELECT 1', 'show tables', 'EXPLAIN SELECT 1', 'use db; select 1', 'SET @a = 1', '  -- note\nselect 1', '/* x */ select 1', '(select 1) union (select 2)', '', null])
    assert.equal(lib.txReadsOnly(sql), true, String(sql));
});

test('a run that writes', () => {
  for (const sql of ['insert into t values (1)', 'select 1; delete from t', 'call p()', 'with x as (select 1) delete from t', 'select 1 for update', 'select 1 lock in share mode', 'SELECT 1 FOR SHARE'])
    assert.equal(lib.txReadsOnly(sql), false, sql);
});

test('a # or -- or ; inside a string does not hide a write', () => {
  assert.equal(lib.txReadsOnly("select '#'; delete from t"), false);
  assert.equal(lib.txReadsOnly("select '--'; delete from t"), false);
  assert.equal(lib.txReadsOnly('select "#"; update t set a=1'), false);
  assert.equal(lib.txReadsOnly('select `a#b` from t; delete from t'), false);
  assert.equal(lib.txReadsOnly("select 'a;b', 'c'"), true, 'nor makes a read a write');
});

test('"--" is a comment only before a space, as the server reads it', () => {
  assert.equal(lib.txReadsOnly('select 5--1; delete from t'), false);
  assert.equal(lib.txReadsOnly('select 1 -- ; delete from t'), true);
});

test('an executable comment is code', () => {
  assert.equal(lib.txReadsOnly('/*!50000 delete from t */'), false);
  assert.equal(lib.txReadsOnly('select 1 /*!; delete from t */'), false);
});

test('a string ending in a backslash is read both ways the server may read it', () => {
  // with backslash escapes the quote is escaped and the string runs on; without, it ends there and
  // a DELETE follows - so it is not taken as only reading
  assert.equal(lib.txReadsOnly("select 'a\\'; delete from t; -- '"), false);
});

test('the members of an ENUM or SET, as the server writes them', () => {
  assert.deepEqual(lib.parseQuotedOptionList("enum('a','b c','d,e')"), ['a', 'b c', 'd,e']);
  assert.deepEqual(lib.parseQuotedOptionList("set('it''s','x')"), ["it's", 'x']);
  assert.deepEqual(lib.parseQuotedOptionList("enum('a\\\\b')"), ['a\\b'], 'a backslash is written doubled');
  assert.deepEqual(lib.parseQuotedOptionList("ENUM('')"), ['']);
  assert.deepEqual(lib.parseQuotedOptionList('varchar(10)'), []);
});

test('CSV: quoted where it has to be, NULL as its marker', () => {
  assert.equal(lib.bCSV(['a', 'b'], [['x', 'y, z'], ['say "hi"', null], ['two\nlines', '']]),
    'a,b\nx,"y, z"\n"say ""hi""",\\N\n"two\nlines",');
  assert.equal(lib.bCSV(['a'], [['cr\ronly']]), 'a\n"cr\ronly"');
});

test('Markdown: a pipe is escaped and a line break of any kind is a space, so every row stays one row', () => {
  const md = lib.bMD(['a|b', 'c'], [['x|y', 'one\ntwo'], ['win\r\nline', 'old\rmac'], [null, 1]]);
  assert.equal(md, '| a\\|b | c |\n| --- | --- |\n| x\\|y | one two |\n| win line | old mac |\n|  | 1 |\n');
  assert.equal(md.split('\n').length, 6);
  assert.ok(!md.includes('\r'));
});

test('an edited value keeps the Windows line endings it had', () => {
  assert.equal(lib.keepLineEnds('a\r\nb', 'a\nb\nc'), 'a\r\nb\r\nc');
  assert.equal(lib.keepLineEnds('a\nb', 'a\nb\nc'), 'a\nb\nc', 'Unix endings stay Unix');
  assert.equal(lib.keepLineEnds('a\r\nb\nc', 'x\ny'), 'x\ny', 'mixed endings are not guessed at');
  assert.equal(lib.keepLineEnds(null, 'x\ny'), 'x\ny');
});

test('an image is told by its first bytes', () => {
  assert.equal(lib.detectImageMime([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A]), 'image/png');
  assert.equal(lib.detectImageMime([0xFF, 0xD8, 0xFF, 0xE0]), 'image/jpeg');
  assert.equal(lib.detectImageMime([0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50]), 'image/webp');
  assert.equal(lib.detectImageMime([0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x41, 0x56, 0x45]), null, 'RIFF alone is not WebP (WAVE)');
  assert.equal(lib.detectImageMime([0x89, 0x50]), null, 'too short to tell');
});

test('hex that is UTF-8 text, and hex that is not (as the grid holds it, 0x first)', () => {
  assert.equal(lib.hexIsUtf8('0x48656C6C6F'), true);
  assert.equal(lib.hexIsUtf8('0xC3A4'), true, 'ä');
  assert.equal(lib.hexIsUtf8('0xC3'), false, 'a character cut short');
  assert.equal(lib.hexIsUtf8('0xFFFE'), false);
  assert.equal(lib.hexIsUtf8('0xABC'), false, 'an odd number of digits');
});
