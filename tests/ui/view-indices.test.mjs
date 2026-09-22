// Regression tests for the grid's sort/filter ordering (viewIndices in ui/index.html).
//
// The UI is a single HTML file with no module system, so rather than duplicating the function
// here - a copy would happily keep passing while the real one regressed - this pulls the actual
// source out of ui/index.html and evaluates it. viewIndices' only external dependency is T(id),
// which is stubbed with a fixture tab.
//
// Run: node --test tests/ui/     (or npm test)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const html = readFileSync(join(root, 'ui', 'index.html'), 'utf8');

// Lifts `function <name>(...) { ... }` out of the file by matching braces from its opening one.
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

const viewIndicesSrc = extractFunction(html, 'viewIndices');
const rowHasTextSrc = extractFunction(html, 'rowHasText');
const stepSrc = extractFunction(html, 'gridSearchStep');

// Builds viewIndices with a stub T() returning a tab made of the given column values.
function sortColumn(values, { dir = 1, filters = {} } = {}) {
  const tab = {
    rows: values.map(v => [v]),
    filters,
    sortCol: 0,
    sortDir: dir,
  };
  const viewIndices = new Function('T', `${viewIndicesSrc}\nreturn viewIndices;`)(() => tab);
  return viewIndices('t1').map(ri => tab.rows[ri][0]);
}

test('a DECIMAL column sorts numerically even when trailing zeros are present', () => {
  // The regression: parseFloat("1000.10") is 1000.1, so the old per-pair "does it round-trip?"
  // test failed for this value and fell back to string comparison - while "1.37" and "2.74"
  // passed and compared numerically. Mixing the two rules in one sort is an inconsistent
  // comparator, and "1000.10" came back between "1.37" and "2.74".
  const got = sortColumn(['0.00', '1.37', '1000.10', '2.74', '20.00', '999.90']);
  assert.deepEqual(got, ['0.00', '1.37', '2.74', '20.00', '999.90', '1000.10']);
});

test('the same column reversed is exactly the reverse order', () => {
  const asc = sortColumn(['0.00', '1.37', '1000.10', '2.74', '20.00', '999.90']);
  const desc = sortColumn(['0.00', '1.37', '1000.10', '2.74', '20.00', '999.90'], { dir: -1 });
  assert.deepEqual(desc, [...asc].reverse());
});

test('sorted output is monotonic - the comparator defines one consistent order', () => {
  // Values chosen so that string order and numeric order disagree in both directions, and so
  // that roughly half of them fail a parseFloat round-trip.
  const vals = ['9.50', '10.00', '100.10', '2', '0.30', '1000', '99.99', '3.00'];
  const got = sortColumn(vals).map(Number);
  for (let i = 1; i < got.length; i++) {
    assert.ok(got[i - 1] <= got[i], `not ascending at ${i}: ${got.join(', ')}`);
  }
});

test('integers sort numerically, not as text', () => {
  assert.deepEqual(sortColumn(['1', '2', '10', '20', '100']), ['1', '2', '10', '20', '100']);
});

test('a genuinely textual column still sorts as text', () => {
  assert.deepEqual(sortColumn(['banana', 'apple', 'cherry']), ['apple', 'banana', 'cherry']);
});

test('a column that only looks numeric is not treated as numeric', () => {
  // parseFloat("1abc") is 1, which is why the numeric test matches the WHOLE string instead.
  // One non-numeric value puts the entire column on the text path, so the order stays defined.
  const got = sortColumn(['10', '9', '1abc']);
  assert.deepEqual(got, ['10', '1abc', '9']);
});

test('NULLs sort last and do not make the column non-numeric', () => {
  assert.deepEqual(sortColumn(['10.00', null, '2.00']), ['2.00', '10.00', null]);
});

test('filtering still narrows the view', () => {
  const got = sortColumn(['alpha', 'beta', 'alphabet'], { filters: { 0: 'alpha' } });
  assert.deepEqual(got, ['alpha', 'alphabet']);
});

// The toolbar's search: rows holding the text in any column, over whatever columns the rows have.
function search(rows, q, filters = {}) {
  const tab = { rows, filters, sortCol: -1, sortDir: 1, search: q };
  const viewIndices = new Function('T', `${rowHasTextSrc}
${viewIndicesSrc}
return viewIndices;`)(() => tab);
  return viewIndices('t1');
}

test('the search keeps the rows holding the text in any column, ignoring case', () => {
  const rows = [['1', 'Alice', 'Zurich'], ['2', 'Bob', 'Bern'], ['3', 'Carol', 'ZUG'], ['4', null, 'Basel']];
  assert.deepEqual(search(rows, 'zu'), [0, 2]);
  assert.deepEqual(search(rows, '3'), [2]);
  assert.deepEqual(search(rows, 'nobody'), []);
});

test('an empty search keeps every row, and a NULL never matches', () => {
  const rows = [['a', null], ['b', 'null']];
  assert.deepEqual(search(rows, ''), [0, 1]);
  assert.deepEqual(search(rows, 'null'), [1]);
});

test('the search and a column filter must both match', () => {
  const rows = [['alpha', 'x'], ['alpha', 'y'], ['beta', 'x']];
  assert.deepEqual(search(rows, 'x', { 0: 'alpha' }), [0]);
});

// Enter / Shift+Enter in the search box: which cell it goes to next.
function stepper(rows, q, extra = {}) {
  const tab = { rows, filters: {}, sortCol: -1, sortDir: 1, search: q, ...extra };
  const gridFocus = {};
  const make = new Function('T', 'gridFocus', 'gridSetFocus', 'updatePager', '$',
    `${rowHasTextSrc}
${viewIndicesSrc}
${stepSrc}
return gridSearchStep;`);
  const step = make(() => tab, gridFocus, (id, ri, ci) => { gridFocus[id] = { ri, ci }; }, () => {}, () => ({ focus() {} }));
  return dir => { step('t1', dir); const f = gridFocus.t1; return [f.ri, f.ci, ...tab._hitAt]; };
}

test('Enter goes through the matching cells in order, and wraps round', () => {
  const step = stepper([['zug', 'x'], ['y', 'y'], ['a', 'Zurich']], 'zu');
  assert.deepEqual(step(1), [0, 0, 1, 2]);
  assert.deepEqual(step(1), [2, 1, 2, 2]);
  assert.deepEqual(step(1), [0, 0, 1, 2]);
  assert.deepEqual(step(-1), [2, 1, 2, 2]);
});

test('a hidden column is stepped over, and an edit is what is searched', () => {
  const step = stepper([['zu', 'zu', 'old']], 'zu', { hiddenCols: new Set([0]), pending: { upd: { '0:2': 'zulu' } } });
  assert.deepEqual(step(1), [0, 1, 1, 2]);
  assert.deepEqual(step(1), [0, 2, 2, 2]);
});

// The status line's "LIMIT n in the query": which LIMIT counts as the result's own.
const trailingLimit = new Function(`${extractFunction(html, 'trailingLimit')}
return trailingLimit;`)();

test('only a LIMIT at the end of the statement limits the result, and it reads the row count', () => {
  for (const [sql, want] of [
  ['SELECT * FROM t LIMIT 1000', 1000],
  ['select * from t limit 10, 20;', 20],
  ['SELECT * FROM t LIMIT 5 OFFSET 10 ;  ', 5],
  ['SELECT * FROM t WHERE id IN (SELECT id FROM u LIMIT 3)', null],
  ['SELECT * FROM (SELECT 1 LIMIT 3) x', null],
  ['SELECT * FROM t', null],
  ['SELECT limit_col FROM t', null],
]) assert.equal(trailingLimit(sql), want, sql);
});
