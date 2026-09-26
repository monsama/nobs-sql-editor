// The editor's find and replace (ui/index.html): what the box is taken to mean, where it matches,
// and what one or every match is replaced with - plain text as written, a regex with its groups.
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

const { findRegex, findAll, findReplaceText, findReplaceAllText } = new Function(
  ['findRegex', 'findAll', 'findReplaceText', 'findReplaceAllText'].map(n => extractFunction(html, n)).join('\n') +
  '\nreturn {findRegex, findAll, findReplaceText, findReplaceAllText};')();

const find = (q, v, isRe = false, matchCase = false, max = 5000) => { const re = findRegex(q, isRe, matchCase); return re ? findAll(re, v, max) : re; };
// the whole text after replacing the match at [s, e)
const replaceOne = (q, w, v, s, e, isRe = false) => v.slice(0, s) + findReplaceText(findRegex(q, isRe, false), isRe, w, v, s, e) + v.slice(e);

test('an empty box finds nothing, and a regex that does not parse says so', () => {
  assert.equal(findRegex('', false, false), false);
  assert.equal(findRegex('(', true, false), null);
  assert.ok(findRegex('(', false, false) instanceof RegExp, 'as plain text it is only a bracket');
});

test('plain text is matched as written, special characters and all', () => {
  const v = 'select a.* from t where x in (1) and y = $1 or z like "a|b" -- c:\\path';
  for (const q of ['a.*', '(1)', '$1', 'a|b', 'c:\\path', '[', '^', '?']) {
    const at = v.indexOf(q);
    assert.deepEqual(find(q, v), at < 0 ? [] : [[at, at + q.length], ...find(q, v.slice(at + q.length)).map(m => [m[0] + at + q.length, m[1] + at + q.length])], q);
  }
  assert.deepEqual(find('.', 'a.b.c'), [[1, 2], [3, 4]], 'a dot is a dot');
});

test('case matters only when asked', () => {
  assert.deepEqual(find('select', 'SELECT select Select'), [[0, 6], [7, 13], [14, 20]]);
  assert.deepEqual(find('select', 'SELECT select Select', false, true), [[7, 13]]);
});

test('a regex, with ^ and $ at every line', () => {
  assert.deepEqual(find('^\\w+', 'one two\nthree', true), [[0, 3], [8, 13]]);
  assert.deepEqual(find('\\d+$', 'a1\nb22', true), [[1, 2], [4, 6]]);
});

test('an empty match is stepped over, not looped on, and not counted', () => {
  assert.deepEqual(find('x*', 'axxb', true), [[1, 3]]);
  assert.deepEqual(find('^', 'a\nb', true), []);
});

test('matches stop at the most there are room for', () => {
  assert.equal(find('a', 'a'.repeat(100), false, false, 10).length, 10);
});

test('the same regex finds the same matches each time (its position starts over)', () => {
  const re = findRegex('a', false, false);
  assert.deepEqual(findAll(re, 'aa', 5), findAll(re, 'aa', 5));
});

test('replacing one match: plain text as written, a $ is a $', () => {
  assert.equal(replaceOne('x', '$1 & $&', 'a x b', 2, 3), 'a $1 & $& b');
});

test('replacing one match with a regex: its groups, worked out where it is', () => {
  assert.equal(replaceOne('(\\w+)@(\\w+)', '$2 at $1', 'mail bob@home now', 5, 13, true), 'mail home at bob now');
  // ^ sees the line the match is on: the second line's start is a start, even replaced on its own
  assert.equal(replaceOne('^(\\w)', '[$1]', 'ab\ncd', 3, 4, true), 'ab\n[c]d');
  // a lookbehind sees what is before the match
  assert.equal(replaceOne('(?<=a)b', 'X', 'ab cb', 1, 2, true), 'aX cb');
});

test('replacing every match: how many, and the text after', () => {
  assert.deepEqual(findReplaceAllText(findRegex('a', false, false), false, 'b', 'A a x'), { n: 2, text: 'b b x' });
  assert.deepEqual(findReplaceAllText(findRegex('a', false, false), false, '$&$&', 'a'), { n: 1, text: '$&$&' }, 'plain: $ is a $');
  assert.deepEqual(findReplaceAllText(findRegex('(\\d)', true, false), true, '<$1>', 'a1b2'), { n: 2, text: 'a<1>b<2>' });
  assert.deepEqual(findReplaceAllText(findRegex('q', false, false), false, 'z', 'abc'), { n: 0, text: 'abc' });
});

test('replacing every match at a line start puts text before each line (comment out a block)', () => {
  assert.deepEqual(findReplaceAllText(findRegex('^', true, false), true, '-- ', 'a\nb'), { n: 2, text: '-- a\n-- b' });
});
