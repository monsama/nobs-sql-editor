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

// A const the functions read, lifted the same way: from its name to the end of its statement.
function extractConst(src, name) {
  const start = src.indexOf(`const ${name}=`);
  assert.notEqual(start, -1, `const ${name} not found - was it renamed?`);
  return src.slice(start, src.indexOf(';', src.indexOf(name === 'PLAN_SKIP' ? '])' : '}', start)) + 1);
}

// The page's own helpers the plan drawing uses, in their simplest form.
const helpers = `const esc=s=>String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
const clip=(s,n)=>String(s).slice(0,n);const fmtCount=n=>String(n);`;
const lib = new Function(helpers + '\n' + ['PLAN_ACCESS', 'PLAN_STEP', 'PLAN_SKIP'].map(n => extractConst(html, n)).join('\n') + '\n' +
  ['acContext', 'acQ', 'bJSON', 'xlsxCell', 'xlsxCol', 'bXLSX', 'crc32', 'zipStore', 'planNum', 'planTable', 'planItems', 'planNode', 'planHtml'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {acContext, acQ, bJSON, xlsxCell, xlsxCol, bXLSX, crc32, zipStore, planHtml};')();

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

// Explain's picture, from what MySQL 8 and MariaDB 12 answered for the same join.
const MYSQL_PLAN = "{\"query_block\":{\"select_id\":1,\"cost_info\":{\"query_cost\":\"6.05\"},\"ordering_operation\":{\"using_filesort\":false,\"nested_loop\":[{\"table\":{\"table_name\":\"u\",\"access_type\":\"ref\",\"possible_keys\":[\"PRIMARY\"],\"key\":\"PRIMARY\",\"used_key_parts\":[\"Host\"],\"key_length\":\"255\",\"ref\":[\"const\"],\"rows_examined_per_scan\":4,\"rows_produced_per_join\":4,\"filtered\":\"100.00\",\"using_index\":true,\"cost_info\":{\"read_cost\":\"1.25\",\"eval_cost\":\"0.40\",\"prefix_cost\":\"1.65\",\"data_read_per_join\":\"2K\"},\"used_columns\":[\"Host\",\"User\"]}},{\"table\":{\"table_name\":\"d\",\"access_type\":\"ref\",\"possible_keys\":[\"User\"],\"key\":\"User\",\"used_key_parts\":[\"User\"],\"key_length\":\"96\",\"ref\":[\"mysql.u.User\"],\"rows_examined_per_scan\":1,\"rows_produced_per_join\":4,\"filtered\":\"100.00\",\"using_index\":true,\"cost_info\":{\"read_cost\":\"4.00\",\"eval_cost\":\"0.40\",\"prefix_cost\":\"6.05\",\"data_read_per_join\":\"2K\"},\"used_columns\":[\"Host\",\"Db\",\"User\"]}}]}}}";
const MARIADB_PLAN = "{\"query_block\":{\"select_id\":1,\"cost\":0.004679203,\"nested_loop\":[{\"table\":{\"table_name\":\"global_priv\",\"access_type\":\"ref\",\"possible_keys\":[\"PRIMARY\"],\"key\":\"PRIMARY\",\"key_length\":\"765\",\"used_key_parts\":[\"Host\"],\"ref\":[\"const\"],\"loops\":1,\"rows\":2,\"cost\":0.001141041,\"filtered\":100,\"attached_condition\":\"`mysql`.global_priv.Host <=> 'localhost' and `mysql`.global_priv.Host = 'localhost'\",\"using_index\":true}},{\"table\":{\"table_name\":\"d\",\"access_type\":\"ref\",\"possible_keys\":[\"User\"],\"key\":\"User\",\"key_length\":\"384\",\"used_key_parts\":[\"User\"],\"ref\":[\"mysql.global_priv.User\"],\"loops\":2,\"rows\":2,\"cost\":0.003538162,\"filtered\":75}}]}}";

test('the plan draws each table read as a card, with how it is read', () => {
  for (const [name, json] of [['MySQL', MYSQL_PLAN], ['MariaDB', MARIADB_PLAN]]) {
    const h = lib.planHtml(json);
    assert.match(h, /No table is read in full/, name);
    assert.equal((h.match(/class="pcard good"/g) || []).length, 2, name + ': two index lookups');
    assert.ok(h.includes('<div class="pstep">Join</div>'), name + ': the join is a step');
    assert.ok(h.includes('<b>d</b> <span class="pacc">ref - index lookup</span>'), name);
  }
  assert.ok(lib.planHtml(MYSQL_PLAN).includes('Sort (ORDER BY)'), 'MySQL names the sort');
  assert.match(lib.planHtml(MARIADB_PLAN), /75% kept/, 'what a filter keeps, when it drops some');
});

test('a full scan is called out, and a plan that is not JSON is shown as it came', () => {
  const h = lib.planHtml({ query_block: { select_id: 1, table: { table_name: 'big', access_type: 'ALL', rows: 120000, attached_condition: 'a<b' } } });
  assert.match(h, /1 table is read in full: big/);
  assert.match(h, /class="pcard bad"/);
  assert.match(h, /where a&lt;b/);
  assert.match(lib.planHtml('not json'), /did not answer with a plan/);
});

// The chart: which columns are numbers, the axis steps, and what gets drawn.
const chart = new Function(helpers + '\n' + ['chartIsNum', 'chartNumericCols', 'chartTicks', 'chartFmt', 'chartBar', 'chartSvg'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {chartNumericCols, chartTicks, chartFmt, chartSvg};')();

test('the columns of numbers are the ones a chart can draw', () => {
  const rows = [['a', '1', '2.5', null], ['b', '2', 'x', '3'], ['c', '-3', '4', '']];
  assert.deepEqual(chart.chartNumericCols(['name', 'n', 'mixed', 'sparse'], rows), [false, true, false, true]);
});

test('the axis steps are round numbers that cover the data', () => {
  assert.deepEqual(chart.chartTicks(0, 87, 5), [0, 20, 40, 60, 80, 100]);
  assert.deepEqual(chart.chartTicks(-3, 4, 5), [-4, -2, 0, 2, 4]);
  assert.deepEqual(chart.chartTicks(5, 5, 5), [5, 5.2, 5.4, 5.6, 5.8, 6]);
  assert.equal(chart.chartFmt(1234567), '1.23M');
  assert.equal(chart.chartFmt(0.125), '0.125');
});

test('bars stand on zero, one per value, and a line is one path', () => {
  const o = { labels: ['a', 'b', 'c'], series: [{ name: 'n', values: [1, -2, 3] }, { name: 'm', values: [2, null, 1] }], width: 400, height: 240 };
  const bars = chart.chartSvg({ ...o, type: 'bar' });
  assert.equal((bars.match(/class="cmark s1"/g) || []).length, 3);
  assert.equal((bars.match(/class="cmark s2"/g) || []).length, 2, 'a missing value draws nothing');
  assert.equal((bars.match(/class="chit"/g) || []).length, 3, 'one hover target per point');
  const line = chart.chartSvg({ ...o, type: 'line' });
  assert.equal((line.match(/class="cline s1"/g) || []).length, 1);
  assert.equal((line.match(/class="cdot s2"/g) || []).length, 2);
});
