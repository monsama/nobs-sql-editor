// A .sql file opened in a tab is saved back as it came: the editor holds only \n, so the file's own
// line ends and byte order mark are noted on opening (sqlFileOpened) and put back on saving
// (sqlFileText). Both are pulled out of ui/index.html, as in the other tests here.
//
// Run: node --test tests/ui/     (or npm test)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const html = readFileSync(join(root, 'ui', 'index.html'), 'utf8');

function extractFunction(src, name) {
  const start = src.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `function ${name} not found in ui/index.html - was it renamed?`);
  let i = src.indexOf('{', start), depth = 0;
  for (let j = i; j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`unbalanced braces while extracting ${name}`);
}

const { sqlFileText, sqlFileOpened, sqlFileTitle } = new Function(
  ['sqlFileText', 'sqlFileOpened', 'sqlFileTitle'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn { sqlFileText, sqlFileOpened, sqlFileTitle };')();

// What a textarea makes of a value: every line end becomes \n.
const textarea = s => s.replace(/\r\n?/g, '\n');
const roundTrip = (raw, bom) => { const f = sqlFileOpened('q.sql', raw, bom, 'C:\\q.sql'); return sqlFileText(textarea(f.text), f); };

test('a file is written back byte for byte, whatever its line ends and BOM', () => {
  for (const raw of ['SELECT 1;\r\nSELECT 2;\r\n', 'SELECT 1;\nSELECT 2;\n', 'SELECT 1;', '', "SELECT 'a\u00e9\u4e2d\ud83d\ude00';\r\n"]) {
    assert.equal(roundTrip(raw, false), raw, JSON.stringify(raw));
    assert.equal(roundTrip(raw, true), '\uFEFF' + raw, 'with a BOM: ' + JSON.stringify(raw));
  }
});

test('the editor is given \\n only, and the file\'s line end is noted', () => {
  const f = sqlFileOpened('q.sql', 'a\r\nb\r\n', true, null);
  assert.deepEqual([f.text, f.eol, f.bom], ['a\nb\n', '\r\n', true]);
  assert.equal(sqlFileOpened('q.sql', 'a\nb', false).eol, '\n');
});

test('lines typed in the editor get the file\'s line end', () => {
  const f = sqlFileOpened('q.sql', 'a\r\n', false);
  assert.equal(sqlFileText('a\nb\nc', f), 'a\r\nb\r\nc');
  assert.equal(sqlFileText('a\nb', sqlFileOpened('q.sql', 'a\n', false)), 'a\nb');
});

test('the tab is named after the file, without characters a file name cannot have', () => {
  assert.equal(sqlFileTitle('report 2024.sql'), 'report 2024.sql');
  assert.equal(sqlFileTitle('a:b?.sql'), 'a_b_.sql');
  assert.equal(sqlFileTitle(''), 'query.sql');
});
