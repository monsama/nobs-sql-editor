// The editor's autocomplete context and the JSON / Excel exports (ui/index.html).
//
// As in the other tests here, the functions are lifted out of the page and run, rather than
// copied, so a regression in the page is a failure here.
//
// Run: node --test tests/ui/     (or npm test)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { inflateRawSync } from 'node:zlib';
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

const lib = new Function(
  ['acContext', 'acQ', 'bJSON', 'xlsxCell', 'xlsxCol', 'bXLSX', 'crc32', 'zipStore'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {acContext, acQ, bJSON, xlsxCell, xlsxCol, bXLSX, crc32, zipStore};')();

const ctx = sql => lib.acContext(sql.replace('|', ''), sql.indexOf('|'));

test('the tables a statement names, with their aliases', () => {
  const c = ctx('SELECT o.| FROM orders o JOIN customers AS c ON c.id=o.cid');
  assert.deepEqual(c.refs, [{ db: null, table: 'orders', alias: 'o' }, { db: null, table: 'customers', alias: 'c' }]);
  assert.deepEqual(c.qual, [null, 'o']);
  assert.equal(c.word, '');
});

test('a keyword after a table is not its alias', () => {
  assert.deepEqual(ctx('DELETE FROM t WHERE |').refs, [{ db: null, table: 't' }]);
  assert.deepEqual(ctx('UPDATE t SET |').refs, [{ db: null, table: 't' }]);
  assert.deepEqual(ctx('SELECT * FROM a LEFT JOIN b ON |').refs, [{ db: null, table: 'a' }, { db: null, table: 'b' }]);
});

test('schema-qualified and quoted names', () => {
  const c = ctx('SELECT x.na| FROM `my db`.`the table` x');
  assert.deepEqual(c.refs, [{ db: 'my db', table: 'the table', alias: 'x' }]);
  assert.deepEqual(c.qual, [null, 'x']);
  assert.equal(c.word, 'na');
  assert.deepEqual(ctx('SELECT shop.orders.| FROM shop.orders').qual, ['shop', 'orders']);
});

test('the tables after the commas of a FROM list', () => {
  const c = ctx('SELECT | FROM a x, b y, (SELECT 1 FROM z) q WHERE x.id=y.id');
  assert.deepEqual(c.refs.map(r => r.table + ':' + (r.alias || '')), ['a:x', 'z:', 'b:y']);
});

test('only the statement at the caret counts', () => {
  assert.deepEqual(ctx('SELECT * FROM one; SELECT | FROM two; SELECT * FROM three').refs, [{ db: null, table: 'two' }]);
});

test('a table name is due after FROM, JOIN, UPDATE, INTO', () => {
  assert.equal(ctx('SELECT * FROM or|').afterTable, true);
  assert.equal(ctx('SELECT * FROM a JOIN |').afterTable, true);
  assert.equal(ctx('SELECT na| FROM a').afterTable, false);
});

test('names that need quoting get it', () => {
  assert.equal(lib.acQ('plain_name'), 'plain_name');
  assert.equal(lib.acQ('with space'), '`with space`');
  assert.equal(lib.acQ('a`b'), '`a``b`');
});

test('JSON keeps text, NULL and repeated column names', () => {
  const out = JSON.parse(lib.bJSON(['id', 'v', 'id'], [['007', null, '1']]));
  assert.deepEqual(out, [{ id: '007', v: null, id_3: '1' }]);
});

test('Excel numbers only when Excel keeps them exactly', () => {
  assert.equal(lib.xlsxCell('A1', '42', ''), '<c r="A1"><v>42</v></c>');
  assert.equal(lib.xlsxCell('A1', '-3.25', ''), '<c r="A1"><v>-3.25</v></c>');
  assert.match(lib.xlsxCell('A1', '007', ''), /inlineStr/);
  assert.match(lib.xlsxCell('A1', '12345678901234567', ''), /inlineStr/);
  assert.equal(lib.xlsxCell('A1', null, ''), '');
  assert.match(lib.xlsxCell('A1', 'a<b & \u0001c', ''), /<t xml:space="preserve">a&lt;b &amp; c<\/t>/);
  assert.deepEqual([0, 25, 26, 701, 702].map(lib.xlsxCol), ['A', 'Z', 'AA', 'ZZ', 'AAA']);
});

// Reads a zip back: every entry's name, and its bytes checked against the stored CRC.
function unzip(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let e = buf.length - 22;
  assert.equal(dv.getUint32(e, true), 0x06054b50, 'end of central directory');
  const n = dv.getUint16(e + 10, true), out = {};
  let c = dv.getUint32(e + 16, true);
  for (let k = 0; k < n; k++) {
    assert.equal(dv.getUint32(c, true), 0x02014b50);
    const method = dv.getUint16(c + 10, true), crc = dv.getUint32(c + 16, true), size = dv.getUint32(c + 20, true);
    const nl = dv.getUint16(c + 28, true), off = dv.getUint32(c + 42, true);
    const name = new TextDecoder().decode(buf.subarray(c + 46, c + 46 + nl));
    assert.equal(dv.getUint32(off, true), 0x04034b50, 'local header for ' + name);
    const start = off + 30 + dv.getUint16(off + 26, true) + dv.getUint16(off + 28, true);
    let data = buf.subarray(start, start + size);
    if (method === 8) data = inflateRawSync(data);
    assert.equal(lib.crc32(data), crc, 'crc of ' + name);
    out[name] = new TextDecoder().decode(data);
    c += 46 + nl + dv.getUint16(c + 30, true) + dv.getUint16(c + 32, true);
  }
  return out;
}

test('the workbook is a well-formed zip with the parts Excel needs', () => {
  const files = unzip(lib.bXLSX(['id', 'naïve'], [['1', 'x'], ['2', null]]));
  assert.deepEqual(Object.keys(files).sort(), ['[Content_Types].xml', '_rels/.rels', 'xl/_rels/workbook.xml.rels', 'xl/styles.xml', 'xl/workbook.xml', 'xl/worksheets/sheet1.xml']);
  const sheet = files['xl/worksheets/sheet1.xml'];
  assert.match(sheet, /<row r="1"><c r="A1" t="inlineStr" s="1"><is><t xml:space="preserve">id<\/t><\/is><\/c><c r="B1" t="inlineStr" s="1"><is><t xml:space="preserve">naïve<\/t>/);
  assert.match(sheet, /<row r="3"><c r="A3"><v>2<\/v><\/c><\/row>/);
});

test('crc32 matches the standard check value', () => {
  assert.equal(lib.crc32(new TextEncoder().encode('123456789')), 0xCBF43926);
});
