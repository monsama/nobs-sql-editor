// Regression tests for the Server processes dialog's column sort (plSortRows in ui/index.html).
//
// Pulls the real function out of ui/index.html (same approach as view-indices.test.mjs) so the
// test can't keep passing against a stale copy.
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

const plSortRows = new Function(extractFunction(html, 'plSortRows') + '\nreturn plSortRows;')();
const col = (values, dir) => plSortRows(values.map(v => [v]), 0, dir).map(r => r[0]);

test('numeric columns (Id, Time) sort as numbers, not text', () => {
  assert.deepEqual(col(['10', '9', '100', '1'], 1), ['1', '9', '10', '100']);
  assert.deepEqual(col([3, 25, 7], -1), [25, 7, 3]);
});

test('text columns sort case-insensitively', () => {
  assert.deepEqual(col(['Sleep', 'query', 'Daemon'], 1), ['Daemon', 'query', 'Sleep']);
  assert.deepEqual(col(['Sleep', 'query', 'Daemon'], -1), ['Sleep', 'query', 'Daemon']);
});

test('NULL is the lowest value, and empty comes just after it: first going up, last going down', () => {
  assert.deepEqual(col([null, 'b', '', 'a'], 1), [null, '', 'a', 'b']);
  assert.deepEqual(col([null, 'b', '', 'a'], -1), ['b', 'a', '', null]);
  assert.deepEqual(col(['3', null, '10'], 1), [null, '3', '10']);
  assert.deepEqual(col(['3', null, '10'], -1), ['10', '3', null]);
});

test('does not mutate the original rows', () => {
  const rows = [['2'], ['1']];
  plSortRows(rows, 0, 1);
  assert.deepEqual(rows, [['2'], ['1']]);
});
