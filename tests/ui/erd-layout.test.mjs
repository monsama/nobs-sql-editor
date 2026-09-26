// The ER diagram's layout and its lines (ui/index.html): which column each table goes in, the order
// within a column, and how a foreign key's line runs - around the tables in between, never under one.
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

const { erdLayout, erdRoute } = new Function(['erdLayout', 'erdRoute'].map(n => extractFunction(html, n)).join('\n') + '\nreturn {erdLayout, erdRoute};')();

const O = { padX: 40, padY: 36, gapX: 110, gapY: 34 };
const box = { w: 180, h: 100 };
const sizes = names => Object.fromEntries(names.map(n => [n, box]));
// a foreign key row as the server gives it: table, column, referenced table, referenced column
const fk = (a, b) => [a, b + '_id', b, 'id'];
const layout = (names, fks) => erdLayout(names, fks, sizes(names), O);
const lanes = () => { const m = new Map(); return k => { const v = m.get(k) || 0; m.set(k, v + 1); return v; }; };

test('each table right of the tables it refers to', () => {
  const L = layout(['a', 'b', 'c'], [fk('c', 'b'), fk('b', 'a')]);
  assert.ok(L.pos.a.x < L.pos.b.x && L.pos.b.x < L.pos.c.x, JSON.stringify(L.pos));
  assert.equal(L.pos.a.x, O.padX);
  assert.equal(L.pos.b.x - L.pos.a.x, box.w + O.gapX);
});

test('a table goes one column past the furthest table it refers to', () => {
  // d refers to a (column 0) and to c (column 2): it goes in column 3, not 1
  const L = layout(['a', 'b', 'c', 'd'], [fk('b', 'a'), fk('c', 'b'), fk('d', 'a'), fk('d', 'c')]);
  assert.ok(L.pos.d.x > L.pos.c.x);
});

test('tables with no relationship come last, on the right', () => {
  const L = layout(['a', 'b', 'lone'], [fk('b', 'a')]);
  assert.deepEqual(L.names, ['a', 'b', 'lone']);
  assert.ok(L.pos.lone.x > L.pos.b.x);
  const none = layout(['x', 'y'], []);
  assert.equal(none.pos.x.x, O.padX, 'with no relationships at all, they start at the left');
});

test('many tables on their own wrap into further columns', () => {
  const names = Array.from({ length: 12 }, (_, i) => 't' + String(i).padStart(2, '0'));
  const L = layout(names, []);
  const cols = new Set(names.map(n => L.pos[n].x));
  assert.ok(cols.size > 1, 'they do not make one endless column');
  assert.ok(names.every(n => L.pos[n].y + box.h <= Math.max(520, box.h + O.padY) + O.gapY), 'none past the height the linked part (or a screenful) sets');
});

test('within a column, tables sit next to the ones they are linked with', () => {
  // p1 and p2 in the first column; c2 refers to p1, c1 to p2 - so c2 goes above c1, whatever the names say
  const L = layout(['c1', 'c2', 'p1', 'p2'], [fk('c1', 'p2'), fk('c2', 'p1')]);
  assert.ok(L.pos.c2.y < L.pos.c1.y, JSON.stringify(L.pos));
});

test('a cycle of keys and a key to its own table do not hang the layout, and every table is placed', () => {
  const L = layout(['a', 'b', 'self'], [fk('a', 'b'), fk('b', 'a'), ['self', 'parent_id', 'self', 'id']]);
  assert.deepEqual(L.names.slice().sort(), ['a', 'b', 'self']);
  assert.notEqual(L.pos.a.x, L.pos.b.x, 'the two of a cycle get a column each');
});

test('keys to tables not drawn are left out of the layout', () => {
  const L = layout(['a'], [fk('a', 'elsewhere')]);
  assert.deepEqual(L.names, ['a']);
  assert.equal(L.pos.a.x, O.padX);
});

test('the diagram is as large as what it holds', () => {
  const L = layout(['a', 'b'], [fk('b', 'a')]);
  assert.equal(L.totalW, L.pos.b.x + box.w + O.padX);
  assert.equal(L.totalH, O.padY + box.h + O.padY);
});

// the lines
const pos = { p: { x: 40, y: 36, w: 180, h: 100 }, c: { x: 330, y: 36, w: 180, h: 100 }, mid: { x: 330, y: 200, w: 180, h: 100 }, far: { x: 620, y: 36, w: 180, h: 100 } };
const names = Object.keys(pos);
const rightAngled = pts => pts.every((p, i) => !i || p[0] === pts[i - 1][0] || p[1] === pts[i - 1][1]);
const crosses = (pts, b) => pts.slice(1).some((p, i) => { const q = pts[i]; return Math.max(p[0], q[0]) > b.x && Math.min(p[0], q[0]) < b.x + b.w && Math.max(p[1], q[1]) > b.y && Math.min(p[1], q[1]) < b.y + b.h; });

test('side by side: from the key\'s edge to the facing edge, right-angled', () => {
  const r = erdRoute(pos.c, pos.p, 60, 80, false, pos, names, ['c', 'p'], lanes());
  assert.equal(r.x1, pos.c.x); assert.equal(r.d1, -1);
  assert.equal(r.x2, pos.p.x + pos.p.w); assert.equal(r.d2, 1);
  assert.deepEqual(r.pts[0], [r.x1, 60]); assert.deepEqual(r.pts.at(-1), [r.x2, 80]);
  assert.equal(r.pts.length, 4); assert.ok(rightAngled(r.pts));
});

test('lines sharing a gap sit side by side, not on top of each other', () => {
  const lane = lanes();
  const a = erdRoute(pos.c, pos.p, 60, 80, false, pos, names, ['c', 'p'], lane);
  const b = erdRoute(pos.c, pos.p, 100, 60, false, pos, names, ['c', 'p'], lane);
  assert.notEqual(a.pts[1][0], b.pts[1][0]);
});

test('a line across a column goes around the table in its way', () => {
  // far refers to p, two columns left; c sits in between on the same rows
  const r = erdRoute(pos.far, pos.p, 60, 60, false, pos, names, ['far', 'p'], lanes());
  assert.ok(!crosses(r.pts, pos.c), JSON.stringify(r.pts));
  assert.ok(!crosses(r.pts, pos.mid));
  assert.ok(rightAngled(r.pts));
  assert.equal(r.pts.length, 6);
  assert.ok(r.pts[2][1] < pos.c.y, 'over the top, where there is room');
});

test('below the tables when there is no room above', () => {
  const tight = { ...pos, p: { ...pos.p, y: 0 }, c: { ...pos.c, y: 0 }, far: { ...pos.far, y: 0 } };
  const r = erdRoute(tight.far, tight.p, 20, 20, false, tight, names, ['far', 'p'], lanes());
  assert.ok(r.pts[2][1] > tight.c.y + tight.c.h, JSON.stringify(r.pts));
  assert.ok(!crosses(r.pts, tight.c) && !crosses(r.pts, tight.mid));
});

test('a table referring to itself: a loop out of its left edge', () => {
  const r = erdRoute(pos.c, pos.c, 60, 100, true, pos, names, ['c'], lanes());
  assert.equal(r.x1, pos.c.x); assert.equal(r.x2, pos.c.x);
  assert.equal(r.d1, -1); assert.equal(r.d2, -1);
  assert.ok(r.pts[1][0] < pos.c.x);
  assert.ok(rightAngled(r.pts));
});

test('overlapping columns (a table dragged): both ends on the right', () => {
  const o = { a: { x: 40, y: 36, w: 180, h: 100 }, b: { x: 100, y: 300, w: 180, h: 100 } };
  const r = erdRoute(o.a, o.b, 60, 320, false, o, ['a', 'b'], ['a', 'b'], lanes());
  assert.equal(r.d1, 1); assert.equal(r.d2, 1);
  assert.ok(r.pts[1][0] > Math.max(o.a.x + o.a.w, o.b.x + o.b.w));
});
