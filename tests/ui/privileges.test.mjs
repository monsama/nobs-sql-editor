// The Privileges window (ui/index.html): the GRANT and REVOKE statements it writes to take an account
// from what it has to what is ticked, the level they are at, and which grant on a database it edits
// when "_" and "%" in the name are wildcards.
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

// window: which server, and whether it takes a backslash as an escape
const window = { mariadb: false, noBackslashEscapes: false };
const L = new Function('window', ['RESERVED', 'PRIV_TABLE', 'PRIV_DB'].map(n => extractConst(html, n)).join('\n') + '\n' +
  ['qid', 'strLit', 'uRef', 'privSql', 'privOn', 'privDbEscape', 'privGrantees', 'privPickForm', 'privHad'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {PRIV_TABLE, PRIV_DB, uRef, privSql, privOn, privDbEscape, privGrantees, privPickForm, privHad};')(window);

const S = a => new Set(a);
const WHO = "'app'@'%'", ON = '`shop`.*';
const sql = (had, want, o = {}) => L.privSql(WHO, ON, S(had), !!o.hadGO, S(o.shown || [...had, ...want, 'DELETE']), S(want), !!o.go);

test('nothing changed, nothing to run', () => {
  assert.deepEqual(sql(['SELECT', 'INSERT'], ['SELECT', 'INSERT']), []);
});

test('newly ticked is granted, and only that', () => {
  assert.deepEqual(sql(['SELECT'], ['SELECT', 'INSERT', 'UPDATE']), ["GRANT INSERT, UPDATE ON `shop`.* TO 'app'@'%';"]);
});

test('unticked is revoked', () => {
  assert.deepEqual(sql(['SELECT', 'INSERT'], ['SELECT']), ["REVOKE INSERT ON `shop`.* FROM 'app'@'%';"]);
});

test('both at once: the grant first, then the revoke', () => {
  assert.deepEqual(sql(['SELECT', 'INSERT'], ['SELECT', 'UPDATE']), ["GRANT UPDATE ON `shop`.* TO 'app'@'%';", "REVOKE INSERT ON `shop`.* FROM 'app'@'%';"]);
});

test('a privilege held but not shown at this level is left alone', () => {
  assert.deepEqual(sql(['SELECT', 'SUPER'], ['SELECT'], { shown: ['SELECT', 'INSERT'] }), []);
});

test('WITH GRANT OPTION: given with what is granted, or on its own over what is ticked', () => {
  assert.deepEqual(sql(['SELECT'], ['SELECT', 'INSERT'], { go: true }), ["GRANT INSERT ON `shop`.* TO 'app'@'%' WITH GRANT OPTION;"]);
  assert.deepEqual(sql(['SELECT'], ['SELECT'], { go: true }), ["GRANT SELECT ON `shop`.* TO 'app'@'%' WITH GRANT OPTION;"]);
  assert.deepEqual(sql(['SELECT'], ['SELECT'], { go: true, hadGO: true }), [], 'already had');
  assert.deepEqual(sql([], [], { go: true }), [], 'nothing ticked, nothing to give it with');
});

test('taking WITH GRANT OPTION away revokes it on its own', () => {
  assert.deepEqual(sql(['SELECT'], ['SELECT'], { hadGO: true }), ["REVOKE GRANT OPTION ON `shop`.* FROM 'app'@'%';"]);
});

test('the level: every database, one database, one table', () => {
  assert.equal(L.privOn('global', 'shop', 't', null), '*.*');
  assert.equal(L.privOn('db', 'shop', 't', null), 'shop.*');
  assert.equal(L.privOn('db', 'my app', '', null), '`my app`.*');
  assert.equal(L.privOn('table', 'shop', 'order', null), 'shop.`order`', 'a reserved word is quoted');
  assert.equal(L.privOn('table', 'we`ird', 't', null), '`we``ird`.t');
});

test('a database named with _ or % is granted by its exact name where they are wildcards', () => {
  assert.equal(L.privDbEscape('my_app'), 'my\\_app');
  assert.equal(L.privDbEscape('50%'), '50\\%');
  assert.equal(L.privDbEscape('a\\b'), 'a\\\\b', 'a backslash too, or it would escape what follows');
  assert.equal(L.privDbEscape('plain'), 'plain');
  assert.equal(L.privOn('db', 'my_app', '', L.privDbEscape('my_app')), '`my\\_app`.*');
  assert.equal(L.privOn('table', 'my_app', 't', 'my\\_app'), 'my_app.t', 'at table level they are not wildcards');
});

test('an existing grant on the name as a pattern is the one edited', () => {
  const rows = [['SELECT', 'NO', 'my_app'], ['INSERT', 'NO', 'my\\_app']];
  assert.deepEqual(L.privPickForm('my_app', rows), { form: 'my_app', rows: [rows[0]], pattern: true });
  assert.deepEqual(L.privPickForm('my_app', [rows[1]]), { form: 'my\\_app', rows: [rows[1]], pattern: false });
  assert.deepEqual(L.privPickForm('my_app', []), { form: 'my\\_app', rows: [], pattern: false }, 'with none yet, the exact name');
});

test('what is held: USAGE is nothing, and any grantable row means WITH GRANT OPTION', () => {
  const h = L.privHad([['usage', 'NO'], ['select', 'NO'], ['INSERT', 'YES']]);
  assert.deepEqual([...h.had].sort(), ['INSERT', 'SELECT']);
  assert.equal(h.hadGO, true);
  assert.equal(L.privHad([['SELECT', 'NO']]).hadGO, false);
});

test('the account, quoted as the server needs it', () => {
  window.mariadb = false;
  assert.equal(L.uRef({ u: "o'brien", h: '%' }), "'o''brien'@'%'");
  assert.equal(L.uRef({ u: 'a\\b', h: 'localhost' }), "'a\\\\b'@'localhost'");
  window.mariadb = true;
  assert.equal(L.uRef({ u: 'admins', h: '', role: true }), 'admins', 'a MariaDB role by its name');
  window.mariadb = false;
});

test('the grantee as information_schema writes it, for the lookup', () => {
  assert.equal(L.privGrantees({ u: "o'b", h: '%' }), "'''o''''b''@''%''','''o''''b'''");
});

test('the table-level list is part of the database-level one', () => {
  assert.ok(L.PRIV_TABLE.every(p => L.PRIV_DB.includes(p)));
});
