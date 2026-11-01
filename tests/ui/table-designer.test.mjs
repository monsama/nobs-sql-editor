// The table designer's column handling, run against real information_schema rows.
//
// The designer used to rebuild a column from DATA_TYPE plus one length number, so any edit to an
// existing column - even just its comment - rewrote whatever the form does not show. Measured on
// MySQL 8.0.46 from a comment-only edit: DECIMAL(10,2) 12.34 became DECIMAL(10,0) 12, INT UNSIGNED
// became signed, DATETIME(6) lost its microseconds, a latin1 column was re-encoded to utf8mb4, and
// an ENUM became the syntax error ENUM(5). Renaming a column was DROP + ADD, which throws its data
// away. On MariaDB, whose information_schema quotes string defaults, a touched default would have
// gained a second layer of quotes.
//
// FIXTURES below are verbatim information_schema.COLUMNS rows for one deliberately awkward table,
// captured from MySQL 8.0.46 and MariaDB 12.2.2. The same table was then edited through the real
// GUI on both servers, and SHOW CREATE TABLE and the data were identical afterwards.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const html = readFileSync(process.env.NOBS_UI_SOURCE ||
  join(dirname(fileURLToPath(import.meta.url)), '../../ui/index.html'), 'utf8');

function extract(src, name) {
  let start = src.indexOf(`function ${name}(`);
  assert.ok(start >= 0, `${name}() not found`);
  let depth = 0;
  for (let j = src.indexOf('{', start); j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error('unbalanced braces in ' + name);
}
const consts = html.split(/\r?\n/).filter(l => /^const D_(TEXTY|NUMERIC|TEMPORAL|IDXKW)=/.test(l)).join('\n');
const D = new Function(
  'const RESERVED=new Set(["key","order","select"]);\n' + consts + '\n' +
  ['qid', 'strLit', 'lit', 'dColFromInfo', 'dCsOf', 'colDef', 'dIdList', 'dIdName', 'dIdText', 'dIdSql', 'dRefTbl', 'dRefText', 'dIdxFromStats', 'dFksFromInfo', 'dIdxDef', 'dFkDef', 'dOptSql', 'dStay', 'dAlterSql', 'dCreateSql', 'sqlBlankStringsAndComments', 'dColumnLines', 'dKeepFromCreate', 'dTsNotes'].map(n => extract(html, n)).join('\n') +
  '\nreturn {dColFromInfo,colDef,dAlterSql,dCreateSql,dColumnLines,dKeepFromCreate,dTsNotes,dIdList,dIdText,dRefTbl,dIdxFromStats,dFksFromInfo,dIdxDef,dFkDef,dOptSql};')();

const FIXTURES = [
  {version:"8.0.46", mariadb:false, tableColl:"utf8mb4_0900_ai_ci", rows:[
    ["id","int","int","NO",null,"auto_increment","PRI","",null,null,""],
    ["amount","decimal","decimal(10,2)","NO","0.00","","","",null,null,""],
    ["big","int","int unsigned","NO",null,"","","",null,null,""],
    ["zf","smallint","smallint(5) unsigned zerofill","YES",null,"","","",null,null,""],
    ["ts","datetime","datetime(6)","YES","CURRENT_TIMESTAMP(6)","DEFAULT_GENERATED on update CURRENT_TIMESTAMP(6)","","",null,null,""],
    ["kind","enum","enum('al)pha','beta')","NO","beta","","","","utf8mb4","utf8mb4_0900_ai_ci",""],
    ["tags","set","set('x','y')","YES",null,"","","","utf8mb4","utf8mb4_0900_ai_ci",""],
    ["name","varchar","varchar(20)","YES","it's ok","","","","latin1","latin1_german2_ci",""],
    ["empty_def","varchar","varchar(5)","NO","","","","","utf8mb4","utf8mb4_0900_ai_ci",""],
    ["hidden","int","int","YES",null,"INVISIBLE","","",null,null,""],
    ["gen","bigint","bigint","YES",null,"VIRTUAL GENERATED","","",null,null,"(`big` + 1)"],
    ["uid","char","char(36)","YES","uuid()","DEFAULT_GENERATED","","","utf8mb4","utf8mb4_0900_ai_ci",""],
    ["tt","tinytext","tinytext","YES",null,"","","","utf8mb4","utf8mb4_0900_ai_ci",""],
    ["bl","mediumblob","mediumblob","YES",null,"","","",null,null,""],
    ["bits","bit","bit(3)","YES","b'101'","","","",null,null,""],
    ["f","double","double unsigned","YES",null,"","","",null,null,""]]},
  {version:"12.2.2-MariaDB", mariadb:true, tableColl:"utf8mb4_uca1400_ai_ci", rows:[
    ["id","int","int(11)","NO",null,"auto_increment","PRI","",null,null,null],
    ["amount","decimal","decimal(10,2)","NO","0.00","","","",null,null,null],
    ["big","int","int(10) unsigned","NO",null,"","","",null,null,null],
    ["zf","smallint","smallint(5) unsigned zerofill","YES","NULL","","","",null,null,null],
    ["ts","datetime","datetime(6)","YES","current_timestamp(6)","on update current_timestamp(6)","","",null,null,null],
    ["kind","enum","enum('al)pha','beta')","NO","'beta'","","","","utf8mb4","utf8mb4_uca1400_ai_ci",null],
    ["tags","set","set('x','y')","YES","NULL","","","","utf8mb4","utf8mb4_uca1400_ai_ci",null],
    ["name","varchar","varchar(20)","YES","'it''s ok'","","","","latin1","latin1_german2_ci",null],
    ["empty_def","varchar","varchar(5)","NO","''","","","","utf8mb4","utf8mb4_uca1400_ai_ci",null],
    ["hidden","int","int(11)","YES","NULL","INVISIBLE","","",null,null,null],
    ["gen","bigint","bigint(20)","YES","NULL","VIRTUAL GENERATED","","",null,null,"`big` + 1"],
    ["uid","char","char(36)","YES","uuid()","","","","utf8mb4","utf8mb4_uca1400_ai_ci",null],
    ["tt","tinytext","tinytext","YES","NULL","","","","utf8mb4","utf8mb4_uca1400_ai_ci",null],
    ["bl","mediumblob","mediumblob","YES","NULL","","","",null,null,null],
    ["bits","bit","bit(3)","YES","b'101'","","","",null,null,null],
    ["f","double","double unsigned","YES","NULL","","","",null,null,null]]}
];

const read = (f) => f.rows.map(r => D.dColFromInfo(r, f.mariadb, f.tableColl));
const byName = (cols, n) => cols.find(c => c.name === n);
const clone = (cols) => cols.map(c => ({ ...c, keep: c.keep }));
const TBL = 'nobs_gui.awk';

for (const f of FIXTURES) {
  const label = f.mariadb ? `MariaDB ${f.version}` : `MySQL ${f.version}`;

  test(`${label}: every column is written back with what the form does not show`, () => {
    const cols = read(f);
    const def = n => D.colDef(byName(cols, n));
    assert.match(def('amount'), /^amount DECIMAL\(10,2\) NOT NULL DEFAULT /, 'the DECIMAL scale was lost');
    assert.match(def('big'), /^big INT(\(10\))? UNSIGNED NOT NULL$/, 'UNSIGNED was lost');
    assert.match(def('zf'), /^zf SMALLINT\(5\) UNSIGNED ZEROFILL/, 'ZEROFILL was lost');
    assert.match(def('ts'), /^ts DATETIME\(6\) DEFAULT current_timestamp\(6\) ON UPDATE current_timestamp\(6\)$/i,
      'fractional seconds, the expression default or ON UPDATE was lost');
    assert.match(def('kind'), /^kind ENUM\('al\)pha','beta'\) NOT NULL DEFAULT 'beta'$/,
      'the ENUM value list was lost or cut at the ) inside a value');
    assert.match(def('tags'), /^tags SET\('x','y'\)/);
    assert.match(def('name'), /^name VARCHAR\(20\) CHARACTER SET latin1 COLLATE latin1_german2_ci DEFAULT 'it''s ok'$/,
      'a non-default character set, or the quoted default, was not kept exactly');
    assert.match(def('empty_def'), /^empty_def VARCHAR\(5\) NOT NULL DEFAULT ''$/,
      'an empty-string default is a default, not "no default"');
    assert.match(def('hidden'), /INVISIBLE$/);
    assert.match(def('uid'), /^uid CHAR\(36\) DEFAULT \(?uuid\(\)\)?$/, 'the expression default was quoted into a string');
    assert.match(def('bits'), /^bits BIT\(3\) DEFAULT b'101'$/, 'a bit default was quoted into a string');
    assert.match(def('f'), /^f DOUBLE UNSIGNED/);
    // Columns that merely inherit the table's collation must not grow a clause of their own, or
    // SHOW CREATE TABLE - and Compare DB - would read them as changed.
    for (const n of ['kind', 'tags', 'empty_def', 'uid', 'tt'])
      assert.doesNotMatch(def(n), /CHARACTER SET/, `${n} inherits the table collation and must not get a clause`);
  });

  test(`${label}: the form shows defaults as a person would type them`, () => {
    const cols = read(f);
    assert.equal(byName(cols, 'name').def, "it's ok", 'a quoted default should be shown unquoted');
    assert.equal(byName(cols, 'kind').def, 'beta');
    assert.equal(byName(cols, 'empty_def').def, '');
  });

  test(`${label}: an untouched table produces nothing`, () => {
    const cols = read(f);
    assert.equal(D.dAlterSql(cols, clone(cols), TBL), '-- no changes detected');
  });

  test(`${label}: a comment-only edit changes comments and nothing else`, () => {
    const orig = read(f);
    const edited = clone(orig).map(c => ({ ...c, comment: 'c-' + c.name }));
    const sql = D.dAlterSql(orig, edited, TBL);
    for (const c of orig) {
      if (c.keep.generated) continue;
      const line = sql.split('\n').find(l => l.includes('MODIFY COLUMN ' + c.name + ' '));
      assert.ok(line, `${c.name} was not modified`);
      assert.equal(line.replace(/^\s*MODIFY COLUMN /, '').replace(/,$|;$/, ''),
        D.colDef({ ...c, comment: 'c-' + c.name }), `${c.name}: the MODIFY is not the column as it was, plus the comment`);
    }
    // A generated column is left alone, and the SQL says so.
    assert.match(sql, /^-- gen is a generated column/m);
    assert.doesNotMatch(sql, /COLUMN gen /);
  });

  test(`${label}: renaming a column is CHANGE COLUMN, never DROP + ADD`, () => {
    const orig = read(f);
    const edited = clone(orig).map(c => c.name === 'name' ? { ...c, name: 'full_name' } : c);
    const sql = D.dAlterSql(orig, edited, TBL);
    assert.match(sql, /CHANGE COLUMN name full_name VARCHAR\(20\) CHARACTER SET latin1/);
    assert.doesNotMatch(sql, /DROP COLUMN/, 'a rename must not drop the column and its data');
    assert.doesNotMatch(sql, /ADD COLUMN/);
    // The primary key is not disturbed by renaming it either.
    const renPk = clone(orig).map(c => c.name === 'id' ? { ...c, name: 'row_id' } : c);
    const pkSql = D.dAlterSql(orig, renPk, TBL);
    assert.match(pkSql, /CHANGE COLUMN id row_id INT/);
    assert.doesNotMatch(pkSql, /PRIMARY KEY/, 'renaming the key column must not drop and re-add the key');
  });

  test(`${label}: removed and added columns are still dropped and added`, () => {
    const orig = read(f);
    const edited = clone(orig).filter(c => c.name !== 'tt');
    edited.push({ name: 'extra', type: 'VARCHAR', len: '10', nn: false, ai: false, pk: false, def: '', comment: '' });
    const sql = D.dAlterSql(orig, edited, TBL);
    assert.match(sql, /DROP COLUMN tt/);
    assert.match(sql, /ADD COLUMN extra VARCHAR\(10\)/);
  });

  test(`${label}: changing a type drops only what no longer applies`, () => {
    const orig = read(f);
    const f2 = { ...byName(orig, 'f'), type: 'VARCHAR', len: '20' };
    assert.doesNotMatch(D.colDef(f2), /UNSIGNED/, 'UNSIGNED on a VARCHAR is a syntax error');
    const n2 = { ...byName(orig, 'name'), type: 'INT', len: '' };
    assert.doesNotMatch(D.colDef(n2), /CHARACTER SET/, 'a character set on an INT is a syntax error');
    const ts2 = { ...byName(orig, 'ts'), type: 'DATE', len: '' };
    assert.doesNotMatch(D.colDef(ts2), /ON UPDATE/, 'ON UPDATE only applies to DATETIME and TIMESTAMP');
    const big2 = { ...byName(orig, 'big'), type: 'BIGINT' };
    assert.match(D.colDef(big2), /BIGINT(\(10\))? UNSIGNED/, 'widening an unsigned integer should keep it unsigned');
  });

  test(`${label}: a default the user typed is still written the usual way`, () => {
    const orig = read(f);
    const c = { ...byName(orig, 'empty_def'), def: "o'k" };
    assert.match(D.colDef(c), /DEFAULT 'o''k'$/);
    const n = { ...byName(orig, 'amount'), def: '5' };
    assert.match(D.colDef(n), /DEFAULT 5$/);
    // a text column's digits are text; an expression in brackets and a bit literal are SQL
    assert.match(D.colDef({ ...c, def: '007' }), /DEFAULT '007'$/);
    assert.match(D.colDef({ ...c, def: '0x41' }), /DEFAULT '0x41'$/);
    assert.match(D.colDef({ ...c, def: '(uuid())' }), /DEFAULT \(uuid\(\)\)$/);
    assert.match(D.colDef({ ...byName(orig, 'amount'), def: '1e3' }), /DEFAULT 1e3$/);
    assert.match(D.colDef({ ...byName(orig, 'ts'), def: '(CURDATE())' }), /DEFAULT \(CURDATE\(\)\)/);
  });
}

// information_schema does not carry a column-level CHECK (MariaDB writes json_valid for every JSON
// column) or MySQL's SRID; a MODIFY without them dropped both. They are taken from SHOW CREATE
// TABLE and written back while the column keeps its type.
test('a CHECK and an SRID from the CREATE line survive a MODIFY of the column', () => {
  const create = 'CREATE TABLE `t` (\n  `j` longtext CHARACTER SET utf8mb4 COLLATE utf8mb4_bin DEFAULT NULL COMMENT \'see CHECK (x)\' CHECK (json_valid(`j`)),\n' +
    '  `g` point NOT NULL /*!80003 SRID 4326 */,\n  `c` varchar(5) DEFAULT NULL COMMENT \'CHECK (1)\'\n)';
  const lines = D.dColumnLines(create);
  const col = (name, type) => { const c = { name, type, len: '', nn: false, ai: false, def: null, comment: 'x', keep: { origName: name, origType: type } }; D.dKeepFromCreate(c, lines[name]); return c; };
  const j = col('j', 'LONGTEXT'), g = col('g', 'POINT'), c = col('c', 'VARCHAR');
  assert.match(D.colDef(j), /COMMENT 'x' CHECK \(json_valid\(`j`\)\)$/);
  assert.match(D.colDef(g), /\/\*!80003 SRID 4326 \*\//);
  assert.doesNotMatch(D.colDef(c), /CHECK/, 'a comment that mentions CHECK is not a constraint');
  assert.doesNotMatch(D.colDef({ ...j, type: 'TEXT' }), /CHECK/, 'a new type is written as asked');
});

// The key is read as the table has it, in its own order: a UNIQUE NOT NULL column that COLUMN_KEY
// reports as PRI is not in it, and adding a column keeps the order of the ones already there.
test('the primary key keeps its own columns and order', () => {
  const mk = (name, pk) => ({ name, type: 'INT', len: '', nn: true, ai: false, pk, def: null, comment: '', keep: { origName: name, origType: 'INT' } });
  const orig = [mk('a', true), mk('b', true), mk('c', false)]; orig.pkOrder = ['b', 'a'];
  assert.match(D.dAlterSql(orig, [mk('a', true), mk('b', true), mk('c', true)], 't'), /ADD PRIMARY KEY \(b,a,c\)/);
  assert.match(D.dAlterSql(orig, orig.map(c => ({ ...c })), 't'), /no changes/);
  const uniq = [mk('u', false), mk('x', false)]; uniq.pkOrder = [];
  const sql = D.dAlterSql(uniq, [mk('u', false), mk('x', true)], 't');
  assert.doesNotMatch(sql, /DROP PRIMARY KEY/, 'there was no primary key to drop');
  assert.match(sql, /ADD PRIMARY KEY \(x\)/);
});

test('NOT NULL is not written with DEFAULT NULL', () => {
  const c = { name: 'v', type: 'VARCHAR', len: '5', nn: true, ai: false, def: 'NULL', comment: '', keep: { origName: 'v', origType: 'VARCHAR', defShown: 'NULL', defSql: 'NULL' } };
  assert.doesNotMatch(D.colDef(c), /DEFAULT NULL/);
  assert.match(D.colDef({ ...c, nn: false }), /DEFAULT NULL/);
});

// Defaults as information_schema gives them where there is no DEFAULT_GENERATED (MySQL 5.7), and a
// MySQL 8 expression default holding a string, whose quotes information_schema escapes.
test('a CURRENT_TIMESTAMP default and an expression holding a string are written back as SQL', () => {
  const row = (name, t, ct, def, extra) => [name, t, ct, 'YES', def, extra, '', '', null, null, ''];
  const ts57 = D.colDef(D.dColFromInfo(row('ts', 'timestamp', 'timestamp', 'CURRENT_TIMESTAMP', 'on update CURRENT_TIMESTAMP'), false, null));
  assert.match(ts57, /DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP/i, ts57);
  assert.doesNotMatch(ts57, /DEFAULT 'CURRENT_TIMESTAMP'/i, 'the MySQL 5.7 default was quoted as text');
  const text = D.colDef(D.dColFromInfo(row('t', 'varchar', 'varchar(20)', 'CURRENT_TIMESTAMP', ''), false, null));
  assert.match(text, /DEFAULT 'CURRENT_TIMESTAMP'/, 'in a text column it is the text');
  const expr = D.colDef(D.dColFromInfo(row('s', 'varchar', 'varchar(20)', "concat(_utf8mb4\\'a\\',_utf8mb4\\'b\\')", 'DEFAULT_GENERATED'), false, null));
  assert.match(expr, /DEFAULT \(concat\(_utf8mb4'a',_utf8mb4'b'\)\)/, expr);
});

// With explicit_defaults_for_timestamp OFF, a TIMESTAMP NOT NULL written without a default may get
// DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP, and no column definition can prevent it
// except a default of its own; the generated SQL says so.
test('a TIMESTAMP NOT NULL without a default is called out when the server would give it one', () => {
  const ts = { name: 't', type: 'TIMESTAMP', len: '', nn: true, ai: false, def: '', comment: '', keep: { origName: 't', origType: 'TIMESTAMP' } };
  assert.deepEqual(D.dTsNotes([ts]), [], 'nothing is said while the setting is unknown or ON');
  globalThis.window = { _dTsAuto: true };
  try {
    assert.equal(D.dTsNotes([ts]).length, 1);
    assert.equal(D.dTsNotes([{ ...ts, def: '2000-01-01 00:00:00' }]).length, 0, 'a default of its own prevents it');
    assert.equal(D.dTsNotes([{ ...ts, nn: false }]).length, 0);
  } finally { delete globalThis.window; }
});

test('the fixtures cover both servers', () => {
  assert.deepEqual(FIXTURES.map(f => f.mariadb).sort(), [false, true]);
});

// ---- Column order, UNSIGNED, collations, indexes, foreign keys, table options ----

const mkc = (name, extra = {}) => ({ name, type: 'INT', len: '', nn: false, ai: false, pk: false, def: null, comment: '', un: false, coll: '', keep: { origName: name, origType: 'INT' }, ...extra });

// How the server builds the new column list from one ALTER (mysql_prepare_alter_table, the same on
// MySQL and MariaDB): the columns that stay put keep their order, renamed in place; then each ADD
// and each column given FIRST / AFTER is put in, in the order of the clauses.
function applyOrder(names, sql) {
  const clauses = sql.replace(/^ALTER TABLE \S+\n\s*/, '').replace(/;$/, '').split(/,\n\s*/);
  const place = [], drop = new Set(), ren = new Map();
  for (const c of clauses) {
    let m;
    if ((m = /^DROP COLUMN (\S+)/.exec(c))) { drop.add(m[1]); continue; }
    if ((m = /^ADD COLUMN (\S+)/.exec(c))) { place.push({ from: null, name: m[1], pos: /( FIRST| AFTER (\S+))$/.exec(c) }); continue; }
    if ((m = /^(?:MODIFY COLUMN (\S+)|CHANGE COLUMN (\S+) (\S+))/.exec(c))) {
      const from = m[1] || m[2], to = m[1] || m[3], pos = /( FIRST| AFTER (\S+))$/.exec(c);
      if (pos) place.push({ from, name: to, pos }); else ren.set(from, to);
    }
  }
  const moved = new Set(place.map(p => p.from).filter(Boolean));
  const list = names.filter(n => !drop.has(n) && !moved.has(n)).map(n => ren.get(n) || n);
  for (const p of place) {
    if (!p.pos) list.push(p.name);
    else if (p.pos[1] === ' FIRST') list.unshift(p.name);
    else { const at = list.indexOf(p.pos[2]); assert.ok(at >= 0, `AFTER ${p.pos[2]} names a column not there yet:\n${sql}`); list.splice(at + 1, 0, p.name); }
  }
  return list;
}
const perms = a => a.length <= 1 ? [a] : a.flatMap((x, i) => perms([...a.slice(0, i), ...a.slice(i + 1)]).map(p => [x, ...p]));

test('every reordering of five columns comes out in the order asked for, moving as few as can be', () => {
  const names = ['a', 'b', 'c', 'd', 'e'], orig = names.map(n => mkc(n));
  for (const p of perms(names)) {
    const sql = D.dAlterSql(orig, p.map(n => mkc(n)), 't');
    if (p.join() === names.join()) { assert.match(sql, /no changes/); continue; }
    assert.deepEqual(applyOrder(names, sql), p, sql);
    // as few as can be: all but the longest run already in order
    let best = 0; const lis = Array(p.length).fill(1);
    for (let j = 0; j < p.length; j++) { for (let k = 0; k < j; k++) if (p[k] < p[j]) lis[j] = Math.max(lis[j], lis[k] + 1); best = Math.max(best, lis[j]); }
    assert.equal((sql.match(/MODIFY COLUMN/g) || []).length, p.length - best, sql);
  }
});

test('moving, renaming, adding and dropping at once still lands in the order asked for', () => {
  const names = ['a', 'b', 'c', 'd'], orig = names.map(n => mkc(n));
  const want = [mkc('x', { keep: null }), { ...mkc('d'), name: 'dd' }, mkc('a'), mkc('y', { keep: null }), { ...mkc('c'), name: 'cc' }, mkc('z', { keep: null })];
  const sql = D.dAlterSql(orig, want, 't');
  assert.match(sql, /DROP COLUMN b/);
  assert.deepEqual(applyOrder(names, sql), ['x', 'dd', 'a', 'y', 'cc', 'z'], sql);
  // new columns at the end are simply added, without a position
  const tail = D.dAlterSql(orig, [...orig, mkc('n', { keep: null })], 't');
  assert.match(tail, /ADD COLUMN n INT;$/);
});

test('UNSIGNED can be ticked and unticked; ZEROFILL goes with it', () => {
  const f = FIXTURES[0], cols = read(f);
  const zf = byName(cols, 'zf'), id = byName(cols, 'id');
  assert.equal(zf.un, true);
  assert.match(D.colDef({ ...zf, un: false }), /^zf SMALLINT\(5\)( DEFAULT NULL)?$/);
  assert.match(D.colDef({ ...id, un: true }), /^id INT UNSIGNED NOT NULL AUTO_INCREMENT$/);
  assert.match(D.dAlterSql(cols, clone(cols).map(c => c.name === 'id' ? { ...c, un: true } : c), TBL), /MODIFY COLUMN id INT UNSIGNED/);
});

test('a column\'s collation can be changed, and one that inherits keeps inheriting', () => {
  for (const f of FIXTURES) {
    const cols = read(f);
    const kind = byName(cols, 'kind');
    assert.equal(kind.coll, f.tableColl, 'the form shows the collation a column has, inherited or not');
    assert.match(D.colDef({ ...kind, coll: 'latin1_swedish_ci' }), / CHARACTER SET latin1 COLLATE latin1_swedish_ci/);
    const name = byName(cols, 'name');
    assert.match(D.colDef({ ...name, coll: f.tableColl }), /^name VARCHAR\(20\) DEFAULT/, 'set to the table\'s own, it inherits again');
    const sql = D.dAlterSql(cols, clone(cols).map(c => c.name === 'tt' ? { ...c, coll: 'utf8mb4_bin' } : c), TBL);
    assert.match(sql, /MODIFY COLUMN tt TINYTEXT CHARACTER SET utf8mb4 COLLATE utf8mb4_bin/);
    assert.equal((sql.match(/COLUMN/g) || []).length, 1, sql);
  }
});

test('a new table collation is written, and a column that had the old one keeps it when it is changed', () => {
  const f = FIXTURES[1], cols = read(f), optOrig = { coll: f.tableColl, engine: 'InnoDB', comment: '' };
  const x = { opt: { ...optOrig, coll: 'utf8mb4_unicode_ci' }, optOrig };
  const sql = D.dAlterSql(cols, clone(cols).map(c => c.name === 'kind' ? { ...c, comment: 'k' } : c), TBL, x);
  assert.match(sql, /DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci/);
  assert.match(sql, new RegExp('MODIFY COLUMN kind ENUM.* COLLATE ' + f.tableColl));
  assert.equal(D.dAlterSql(cols, clone(cols), TBL, { opt: { ...optOrig }, optOrig }), '-- no changes detected');
});

test('key part lists are read and written with prefixes, DESC and quoted names', () => {
  assert.deepEqual(D.dIdList(' a , `b c`(10) DESC,`x``y` asc'), [{ name: 'a', len: '', desc: false }, { name: 'b c', len: '10', desc: true }, { name: 'x`y', len: '', desc: false }]);
  assert.equal(D.dIdText(D.dIdList('a,`b c`(10) desc')), 'a, `b c`(10) DESC');
  assert.deepEqual(D.dRefTbl('`my db`.`t.1`', 'x'), { db: 'my db', name: 't.1' });
  assert.deepEqual(D.dRefTbl('parent', 'x'), { db: 'x', name: 'parent' });
});

// STATISTICS as SELECT * gives it; the columns differ by server, so they are looked up by name.
const STAT_COLS = ['TABLE_SCHEMA', 'TABLE_NAME', 'NON_UNIQUE', 'INDEX_NAME', 'SEQ_IN_INDEX', 'COLUMN_NAME', 'COLLATION', 'SUB_PART', 'INDEX_TYPE', 'INDEX_COMMENT', 'IS_VISIBLE', 'EXPRESSION'];
const STATS = [
  ['d', 't', '0', 'PRIMARY', 1, 'id', 'A', null, 'BTREE', '', 'YES', null],
  ['d', 't', '1', 'ix_ab', 1, 'a', 'A', null, 'BTREE', 'by a', 'YES', null],
  ['d', 't', '1', 'ix_ab', 2, 'b', 'D', '10', 'BTREE', 'by a', 'YES', null],
  ['d', 't', '0', 'uq_c', 1, 'c', 'A', null, 'BTREE', '', 'NO', null],
  ['d', 't', '1', 'ft', 1, 'b', null, null, 'FULLTEXT', '', 'YES', null],
  ['d', 't', '1', 'fx', 1, null, 'A', null, 'BTREE', '', 'YES', 'lower(`b`)'],
];

test('indexes are read from STATISTICS, primary key left out, and an untouched list changes nothing', () => {
  const idx = D.dIdxFromStats(STAT_COLS, STATS);
  assert.deepEqual(idx.map(i => [i.name, i.kind, i.cols]), [['ix_ab', 'INDEX', 'a, b(10) DESC'], ['uq_c', 'UNIQUE', 'c'], ['ft', 'FULLTEXT', 'b'], ['fx', 'INDEX', '(lower(`b`))']]);
  assert.equal(D.dIdxDef(idx[1]), 'UNIQUE INDEX uq_c (c) INVISIBLE', 'an invisible index stays invisible');
  assert.equal(D.dIdxDef(idx[0]), "INDEX ix_ab (a,b(10) DESC) COMMENT 'by a'");
  const cols = ['id', 'a', 'b', 'c'].map(n => mkc(n));
  assert.match(D.dAlterSql(cols, cols.map(c => ({ ...c })), 't', { idx: idx.map(i => ({ ...i })), idxOrig: idx }), /no changes/);
});

test('an index changed, renamed, removed or added is dropped and added as needed', () => {
  const idx = D.dIdxFromStats(STAT_COLS, STATS), cols = ['id', 'a', 'b', 'c'].map(n => mkc(n));
  const run = cur => D.dAlterSql(cols, cols.map(c => ({ ...c })), 't', { idx: cur, idxOrig: idx });
  const edited = idx.map(i => i.name === 'ix_ab' ? { ...i, cols: 'a, c' } : { ...i });
  assert.match(run(edited), /DROP INDEX ix_ab,\n\s*ADD INDEX ix_ab \(a,c\) COMMENT 'by a';/);
  assert.match(run(idx.map(i => i.name === 'uq_c' ? { ...i, name: 'uq_c2' } : { ...i })), /DROP INDEX uq_c,\n\s*ADD UNIQUE INDEX uq_c2 \(c\) INVISIBLE/);
  const sql = run([...idx.filter(i => i.name !== 'fx').map(i => ({ ...i })), { name: '', kind: 'UNIQUE', cols: 'a, b(5)', comment: '' }]);
  assert.match(sql, /DROP INDEX fx/);
  assert.match(sql, /ADD UNIQUE INDEX \(a,b\(5\)\)/);
  assert.doesNotMatch(sql, /ix_ab|uq_c|ft/);
});

test('a column renamed on the Columns tab leaves its indexes and keys alone', () => {
  const idx = D.dIdxFromStats(STAT_COLS, STATS), cols = ['id', 'a', 'b', 'c'].map(n => mkc(n));
  const fk = [{ name: 'fk_c', cols: 'c', ref: 'p', refCols: 'id', onDel: 'CASCADE', onUpd: 'RESTRICT', keep: { origName: 'fk_c' } }];
  // the form renames c to cc in the lists too (dRenameRefs); the server does the same by itself
  const sql = D.dAlterSql(cols, cols.map(c => c.name === 'c' ? { ...c, name: 'cc' } : { ...c }), 't', {
    db: 'd', idx: idx.map(i => i.name === 'uq_c' ? { ...i, cols: 'cc' } : { ...i }), idxOrig: idx,
    fk: fk.map(f => ({ ...f, cols: 'cc' })), fkOrig: fk });
  assert.equal(sql, 'ALTER TABLE t\n  CHANGE COLUMN c cc INT;');
});

test('foreign keys: a changed one is dropped in an ALTER of its own first, then added', () => {
  const rows = [['fk_p', 'a', 'd', 'parent', 'x', 'CASCADE', 'RESTRICT'], ['fk_p', 'b', 'd', 'parent', 'y', 'CASCADE', 'RESTRICT'], ['fk_o', 'c', 'other', 'q', 'id', 'NO ACTION', 'SET NULL']];
  const fk = D.dFksFromInfo(rows, 'd');
  assert.deepEqual(fk.map(f => [f.name, f.cols, f.ref, f.refCols, f.onDel, f.onUpd]), [['fk_p', 'a, b', 'parent', 'x, y', 'RESTRICT', 'CASCADE'], ['fk_o', 'c', 'other.q', 'id', 'SET NULL', 'NO ACTION']]);
  assert.equal(D.dFkDef(fk[1], 'd'), 'CONSTRAINT fk_o FOREIGN KEY (c) REFERENCES other.q (id) ON DELETE SET NULL ON UPDATE NO ACTION');
  const cols = ['a', 'b', 'c'].map(n => mkc(n));
  const run = cur => D.dAlterSql(cols, cols.map(c => ({ ...c })), 't', { db: 'd', fk: cur, fkOrig: fk });
  assert.match(run(fk.map(f => ({ ...f }))), /no changes/);
  assert.equal(run(fk.map(f => f.name === 'fk_p' ? { ...f, onDel: 'CASCADE' } : { ...f })),
    'ALTER TABLE t\n  DROP FOREIGN KEY fk_p;\nALTER TABLE t\n  ADD CONSTRAINT fk_p FOREIGN KEY (a,b) REFERENCES parent (x,y) ON DELETE CASCADE ON UPDATE CASCADE;');
  assert.equal(run([fk[1]]), 'ALTER TABLE t\n  DROP FOREIGN KEY fk_p;');
  assert.match(run([...fk, { name: '', cols: 'c', ref: 'parent', refCols: 'x', onDel: '', onUpd: '' }]), /^ALTER TABLE t\n  ADD FOREIGN KEY \(c\) REFERENCES parent \(x\);$/);
});

test('table options and a new name are written only when they change', () => {
  const cols = [mkc('a')], optOrig = { engine: 'InnoDB', coll: 'utf8mb4_general_ci', comment: '' };
  const run = (opt, newTbl) => D.dAlterSql(cols, cols.map(c => ({ ...c })), 'd.t', { opt, optOrig, newTbl });
  assert.match(run({ ...optOrig }, 'd.t'), /no changes/);
  assert.equal(run({ ...optOrig, engine: 'MyISAM', comment: "it's" }, 'd.t2'), "ALTER TABLE d.t\n  ENGINE=MyISAM,\n  COMMENT='it''s',\n  RENAME TO d.t2;");
  assert.match(run({ ...optOrig, coll: 'latin1_swedish_ci' }), /DEFAULT CHARACTER SET latin1 COLLATE latin1_swedish_ci/);
  assert.match(D.dAlterSql(cols, cols, 'd.t', { opt: { ...optOrig, comment: 'x' }, optOrig: { ...optOrig, comment: 'x' } }), /no changes/);
  assert.match(D.dAlterSql(cols, cols, 'd.t', { opt: { ...optOrig, comment: '' }, optOrig: { ...optOrig, comment: 'x' } }), /COMMENT=''/, 'a comment taken out is cleared');
});

test('CREATE TABLE carries the indexes, keys and options', () => {
  const sql = D.dCreateSql([mkc('id', { pk: true, nn: true, ai: true, keep: null }), mkc('p', { keep: null }), mkc('s', { type: 'VARCHAR', len: '10', coll: 'latin1_bin', keep: null })], 'd.t', {
    db: 'd', idx: [{ name: 'ix', kind: 'INDEX', cols: 'p', comment: '' }, { name: '', kind: 'INDEX', cols: ' ', comment: '' }],
    fk: [{ name: 'fk', cols: 'p', ref: 'parent', refCols: 'id', onDel: 'CASCADE', onUpd: '' }],
    opt: { engine: 'InnoDB', coll: 'utf8mb4_bin', comment: 'c' } });
  assert.equal(sql, "CREATE TABLE d.t (\n  id INT NOT NULL AUTO_INCREMENT,\n  p INT,\n  s VARCHAR(10) CHARACTER SET latin1 COLLATE latin1_bin,\n  PRIMARY KEY (id),\n  INDEX ix (p),\n  CONSTRAINT fk FOREIGN KEY (p) REFERENCES parent (id) ON DELETE CASCADE\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin COMMENT='c';");
});
