// The value panel under the grid: how a value is read in each view and encoding, and - the property
// that matters most - that saving from the panel stores exactly the value that was read, changed only
// where it was edited.
//
// The box the panel edits is a text box, and a text box does not hold every value as it is: it gives
// each CR LF and lone CR back as LF. Auto format replaces the whitespace between the parts of JSON,
// XML and HTML with its own. Both are undone on Save. The fake text box below normalises line
// breaks the way a real one does, so the tests see what the app sees.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// NOBS_UI_SOURCE lets the PowerShell edition run this same file against NOBSSQL.ps1, which carries
// the identical UI inline.
const html = readFileSync(process.env.NOBS_UI_SOURCE ||
  join(dirname(fileURLToPath(import.meta.url)), '../../ui/index.html'), 'utf8').replace(/\r\n/g, '\n');

const START = '// --- The value panel', END = '// --- Picking cells';
const hasPanel = html.includes(START);
const skip = hasPanel ? false : 'this edition of the UI has no value panel yet';

function extractFunction(src, name) {
  let start = src.indexOf(`async function ${name}(`);
  if (start === -1) start = src.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `function ${name} not found - was it renamed?`);
  let depth = 0;
  for (let j = src.indexOf('{', start); j < src.length; j++) {
    if (src[j] === '{') depth++;
    else if (src[j] === '}' && --depth === 0) return src.slice(start, j + 1);
  }
  throw new Error(`unbalanced braces while extracting ${name}`);
}
function extractConst(src, name) {
  const start = src.indexOf(`const ${name}=`);
  assert.notEqual(start, -1, `const ${name} not found - was it renamed?`);
  return src.slice(start, src.indexOf(';\n', start) + 1);
}

const EXPORTS = ['vpShow', 'vpLoad', 'vpSave', 'vpRevert', 'vpSet', 'vpWire', 'vpRefresh', 'vpDirty', 'vpBoxValue', 'vpAskDo',
  'vpToFile', 'vpPref', 'vpEncode', 'vpDecode', 'vpCanEncode', 'vpEncs', 'vpJsonLayout', 'vpMarkupLayout', 'vpBoxText',
  'vpRestore', 'vpAutoView', 'vpSame', 'hexToBytes', 'bytesToHex'];
let bundle = '';
if (hasPanel) {
  const block = html.slice(html.indexOf(START), html.indexOf(END));
  const max = html.match(/const MAX_HEXTEXT_BYTES=(\d+)/)[1];
  bundle = `const MAX_HEXTEXT_BYTES=${max};\n` + extractConst(html, 'IMAGE_SIGS') + '\n' +
    ['hexToBytes', 'bytesToHex', 'hexIsUtf8', 'hexToStrictText', 'looksLikePastedHex', 'detectImageMime', 'fmtB', 'esc']
      .map(n => extractFunction(html, n)).join('\n') + '\n' + block +
    `\nreturn {${EXPORTS.join(',')},state:vpState,open:()=>{vpOpen=true;}};`;
}

// --- A page for the panel to live in ---------------------------------------------------------
function el() {
  const s = new Set();
  return { value: '', style: {}, disabled: false, textContent: '', innerHTML: '', title: '', readOnly: false, placeholder: '',
    scrollTop: 0, wrap: '', listeners: {}, focus() {},
    classList: { toggle: (c, on) => { if (on === undefined ? !s.has(c) : on) s.add(c); else s.delete(c); }, add: c => s.add(c), contains: c => s.has(c) },
    addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); } };
}
// A text box's value is what was put in it with every CR LF and lone CR read back as LF.
function textarea() {
  const e = el(); let v = '';
  Object.defineProperty(e, 'value', { get: () => v, set: x => { v = String(x).replace(/\r\n?/g, '\n'); } });
  return e;
}

// One tab holding one result, and the panel open on it.
function panel({ cols, rows, binCols = [], bitCols = [], colTypes, editable = true, readOnly = false, gen = [] }) {
  const t = { id: 't1', db: 'd', table: 't', cols, rows, binCols, bitCols, colTypes,
    pk: editable ? ['id'] : null, pending: editable ? { upd: {}, del: new Set(), ins: [] } : null };
  const els = {}, toasts = [], saved = [], files = [];
  const $ = id => els[id] || (els[id] = id.startsWith('vpx_') ? textarea() : el());
  let f;
  const env = {
    $, T: () => t, tabs: [t], window: { readOnly }, document: {},
    toast: (m, k) => toasts.push((k === true ? 'ERR ' : '') + m), ask: async () => true,
    // As the grid's own: the value goes to the pending edits (or leaves them when it is the stored
    // one), and the grid is drawn again.
    setUpd: (id, ri, ci, v) => { saved.push(v); const k = ri + ':' + ci; if (v === t.rows[ri][ci]) delete t.pending.upd[k]; else t.pending.upd[k] = v; f.vpRefresh('t1'); },
    renderGrid: () => f.vpRefresh('t1'),
    viewIndices: () => t.rows.map((_, i) => i), isGenCol: (id, c) => gen.includes(c),
    togPress: (b, on) => { b.checked = !!on; b.classList.toggle('ison', !!on); }, gridSetFocus: () => {},
    dlBinary: async (blob, name) => { files.push({ name, bytes: new Uint8Array(await blob.arrayBuffer()) }); },
    requestAnimationFrame: fn => fn(),
  };
  const keys = Object.keys(env);
  f = new Function(...keys, bundle)(...keys.map(k => env[k]));
  f.open(); f.vpWire('t1');
  const box = $('vpx_t1');
  return {
    t, f, $, box, toasts, saved, files,
    st: () => f.state.t1,
    // Shows a cell the way the user left this column: in this view, encoding and format.
    show(ri, ci, { view, enc, fmt = true } = {}) { const p = f.vpPref(t, ci); p.view = view || null; p.enc = enc || null; p.fmt = fmt; f.vpLoad('t1', { ri, ci }); },
    // Typing: the box changes, and the page hears of it.
    type(v) { box.value = v; (box.listeners.input || []).forEach(fn => fn()); },
    edit(at, del, ins) { const v = box.value; this.type(v.slice(0, at) + ins + v.slice(at + del)); },
    save() { return f.vpSave('t1'); },
  };
}

const VIEWS = ['binary', 'html', 'json', 'text', 'xml'];
const hx = s => Buffer.from(s, 'utf8').toString('hex');

// --- Encodings --------------------------------------------------------------------------------
test('every byte reads and writes back in Latin-1, and ASCII stops at 127', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  const all = Uint8Array.from({ length: 256 }, (_, i) => i);
  const d = f.vpDecode(all, 'latin1');
  assert.ok(d.exact);
  assert.deepEqual([...f.vpEncode(d.text, 'latin1').bytes], [...all]);
  assert.equal(f.vpDecode(Uint8Array.of(0x41, 0x7f), 'ascii').exact, true);
  assert.equal(f.vpDecode(Uint8Array.of(0x41, 0x80), 'ascii').exact, false);
  assert.equal(f.vpEncode('é', 'ascii').bad, 'é');
});

test('each single-byte charset gives back the bytes it read', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  for (const enc of ['windows-1252', 'iso-8859-2', 'windows-1250', 'windows-1251', 'koi8-r'].filter(e => f.vpEncs().some(x => x[0] === e))) {
    for (let b = 0; b < 256; b++) {
      const d = f.vpDecode(Uint8Array.of(b), enc);
      if (!d.exact) continue;
      assert.deepEqual([...f.vpEncode(d.text, enc).bytes], [b], `${enc} byte ${b}`);
    }
  }
  assert.deepEqual([...f.vpEncode('€', 'windows-1252').bytes], [0x80], 'the euro sign is 0x80 in Windows-1252');
  assert.equal(f.vpEncode('Ā', 'windows-1252').bad, 'Ā');
});

test('UTF-8 and UTF-16 in both byte orders, surrogate pairs included', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  const s = 'Hé€😀你';
  for (const enc of ['utf-8', 'utf-16le', 'utf-16be']) {
    const b = f.vpEncode(s, enc).bytes;
    const d = f.vpDecode(b, enc);
    assert.ok(d.exact, enc); assert.equal(d.text, s, enc);
  }
  assert.deepEqual([...f.vpEncode('A', 'utf-16le').bytes], [0x41, 0]);
  assert.deepEqual([...f.vpEncode('A', 'utf-16be').bytes], [0, 0x41]);
  assert.deepEqual([...f.vpEncode('€', 'utf-8').bytes], [0xe2, 0x82, 0xac]);
});

test('a byte order mark is read as a character, not dropped', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  const d = f.vpDecode(Uint8Array.of(0xef, 0xbb, 0xbf, 0x41), 'utf-8');
  assert.equal(d.text, '﻿A'); assert.ok(d.exact);
});

test('bytes that are not valid in an encoding are not exact, and the multi-byte Asian ones are read-only', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  assert.equal(f.vpDecode(Uint8Array.of(0xff, 0x41), 'utf-8').exact, false);
  assert.equal(f.vpDecode(Uint8Array.of(0x41, 0), 'utf-16le').exact, true);
  assert.equal(f.vpDecode(Uint8Array.of(0x41), 'utf-16le').exact, false, 'an odd byte out');
  for (const enc of ['shift_jis', 'euc-jp', 'gbk', 'big5', 'euc-kr']) assert.equal(f.vpCanEncode(enc), false, enc);
  if (f.vpEncs().some(x => x[0] === 'shift_jis')) assert.equal(f.vpDecode(Uint8Array.of(0x83, 0x65, 0x83, 0x58, 0x83, 0x67), 'shift_jis').text, 'テスト');
});

// --- Laying out -------------------------------------------------------------------------------
const JSONS = [
  '{"id":12345678901234567890,"v":1.0,"s":"a\\"}b{[,:","e":{},"a":[],"n":[1,[2,{"x":null}]],"u":"é😀"}',
  '{ "a" : [ 1 , 2 ] ,\r\n\t"b":true }\r\n',
  '  [1,2]  ',
  '"just a string"',
  '{"a":{"b":{"c":[{"d":[]}]}}}',
];
const MARKUP = [
  ['xml', '<?xml version="1.0"?><r><a x="1>2">t</a><b/>\r\n<c>  sp  </c><!-- c --><![CDATA[<x>]]></r>'],
  ['xml', '  <r>\n    <a>1</a>\n  </r>\n'],
  ['html', '<!DOCTYPE html><html><body><p>Hi <b>there</b></p><script>if(a<b){x()}</script><br><img src="a.png"><pre>  keep\n   me</pre></body></html>'],
  ['html', 'text before <div>x</div> and after'],
  ['xml', 'a < b and not markup'],
];

test('laid-out JSON reads one part to a line, and gives back the value exactly', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  assert.equal(f.vpJsonLayout('{"a":1,"b":[1,2],"c":{}}').text, '{\n  "a": 1,\n  "b": [\n    1,\n    2\n  ],\n  "c": {}\n}');
  for (const s of JSONS) {
    const l = f.vpJsonLayout(s);
    assert.equal(f.vpRestore(l.text, l.segs), s, s);
    for (const sg of l.segs) assert.match(l.text.slice(sg.at, sg.at + sg.len), /^[ \n]*$/, 'only the layout\'s own whitespace is replaced: ' + s);
  }
  assert.ok(f.vpJsonLayout(JSONS[0]).text.includes('12345678901234567890') && f.vpJsonLayout(JSONS[0]).text.includes('1.0'), 'numbers are kept as written');
});

test('laid-out XML and HTML give back the value exactly', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  for (const [kind, s] of MARKUP) {
    const l = f.vpMarkupLayout(s, kind === 'html');
    assert.equal(f.vpRestore(l.text, l.segs), s, s);
  }
  assert.equal(f.vpMarkupLayout('<r><a>t</a><b><c/></b></r>', false).text, '<r>\n  <a>t</a>\n  <b>\n    <c/>\n  </b>\n</r>');
  const h = f.vpMarkupLayout(MARKUP[2][1], true).text;
  assert.ok(h.includes('<script>if(a<b){x()}</script>') && h.includes('<pre>  keep\n   me</pre>'), 'script and pre are kept as they are: ' + h);
});

test('CR LF and lone CR are recorded, and the layout moves with them', { skip }, () => {
  const { f } = panel({ cols: ['id'], rows: [] });
  const s = 'a\r\nb\rc\nd\r\n';
  const b = f.vpBoxText(s, []);
  assert.equal(b.text, 'a\nb\nc\nd\n');
  assert.equal(f.vpRestore(b.text, b.segs), s);
  const j = '{"a":"x\\r\\n",\r\n"b":1}';
  const l = f.vpJsonLayout(j), bb = f.vpBoxText(l.text, l.segs);
  assert.equal(f.vpRestore(bb.text, bb.segs), j);
});

// --- Choosing a view --------------------------------------------------------------------------
test('Auto format picks the view that suits each value', { skip }, () => {
  const p = panel({ cols: ['t', 'b', 'bit', 'j'], binCols: [false, true, true, false], bitCols: [false, false, true, false], colTypes: { j: 'json' },
    rows: [['{"a":1}', '0x00ff10', '0x01', 'x'], ['<?xml version="1.0"?><r/>', hx('<p>hi</p>') && '0x' + hx('<div>hi</div>'), '0x01', null],
      ['<html><body/></html>', '0x' + hx('plain words'), '0x00', null], ['[not json', null, null, null]] });
  const v = (ri, ci) => { p.show(ri, ci); return p.st().view; };
  assert.equal(v(0, 0), 'json'); assert.equal(v(1, 0), 'xml'); assert.equal(v(2, 0), 'html'); assert.equal(v(3, 0), 'text');
  assert.equal(v(0, 1), 'binary', 'bytes that are not text'); assert.equal(v(1, 1), 'html', 'bytes that are HTML'); assert.equal(v(2, 1), 'text');
  assert.equal(v(0, 2), 'binary', 'BIT'); assert.equal(v(0, 3), 'json', 'a JSON column');
  assert.equal(v(3, 1), 'binary', 'NULL in a binary column');
});

test('a view picked by hand stays for the column, until Auto format is turned on again', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', '{"a":1}'], ['2', '<r/>']] });
  p.show(0, 1); assert.equal(p.st().view, 'json');
  p.f.vpSet('t1', 'view', 'text');
  p.f.vpLoad('t1', { ri: 1, ci: 1 }); assert.equal(p.st().view, 'text', 'the next cell of the column');
  p.f.vpSet('t1', 'fmt'); p.f.vpSet('t1', 'fmt');
  assert.equal(p.st().view, 'xml', 'Auto format back on picks again');
});

// --- Saving what was read ---------------------------------------------------------------------
// Every kind of value, in every view, encoding and format: opening it and saving it changes
// nothing, reading the box back gives the value exactly, and an edit at either end changes the
// value only there.
const TEXTS = ['plain', 'line1\r\nline2\rline3\nend', '', null, JSONS[0], JSONS[1], MARKUP[0][1], MARKUP[2][1], '0x41',
  'Grüße 😀 你好 שלום', '\ttab\t', 'nul\u0000inside', '  padded  \r\n'];
const BYTES = ['0x00ff10', '0x', null, '0x89504e470d0a1a0a', '0x' + hx('a\r\nb'), '0x48006900', '0xe9', '0x' + hx(JSONS[1]), '0x' + hx(MARKUP[0][1]), '0x0d0a0d'];

function* cases() {
  for (const v of TEXTS) yield { kind: 'text', v };
  for (const v of BYTES) yield { kind: 'bytes', v };
}

test('a value opened and saved untouched is never written, in any view, encoding or format', { skip }, () => {
  for (const { kind, v } of cases()) {
    const p = panel({ cols: ['id', 'c'], binCols: [false, kind === 'bytes'], rows: [['1', v]] });
    for (const enc of p.f.vpEncs().map(e => e[0])) for (const view of VIEWS) for (const fmt of [true, false]) {
      p.show(0, 1, { view, enc, fmt });
      assert.equal(p.f.vpDirty('t1'), false);
      assert.equal(p.save(), true);
      assert.deepEqual(p.saved, [], `nothing saved: ${kind} ${JSON.stringify(v)} ${view} ${enc} ${fmt}`);
      assert.deepEqual(p.t.pending.upd, {});
    }
  }
});

test('the box reads back as exactly the value, wherever it can be edited', { skip }, () => {
  let editable = 0;
  for (const { kind, v } of cases()) {
    if (v === null) continue;
    const p = panel({ cols: ['id', 'c'], binCols: [false, kind === 'bytes'], rows: [['1', v]] });
    for (const enc of p.f.vpEncs().map(e => e[0])) for (const view of VIEWS) for (const fmt of [true, false]) {
      p.show(0, 1, { view, enc, fmt });
      if (p.box.readOnly) continue;
      editable++;
      const r = p.f.vpBoxValue('t1');
      const what = `${kind} ${JSON.stringify(v)} ${view} ${enc} fmt=${fmt}: ${JSON.stringify(r)}`;
      assert.ok(!r.err, what);
      assert.ok(r.v === v || (kind === 'bytes' && v === '0x' && r.v === ''), what);
    }
  }
  assert.ok(editable > 500, 'most of the combinations can be edited: ' + editable);
});

test('an edit at the start or the end changes the value there and nowhere else', { skip }, () => {
  const concat = (a, b) => { const o = new Uint8Array(a.length + b.length); o.set(a); o.set(b, a.length); return o; };
  for (const { kind, v } of cases()) {
    for (const where of ['start', 'end']) {
      const p = panel({ cols: ['id', 'c'], binCols: [false, kind === 'bytes'], rows: [['1', v]] });
      for (const enc of p.f.vpEncs().map(e => e[0])) for (const view of VIEWS) for (const fmt of [true, false]) {
        p.saved.length = 0; p.t.pending.upd = {};
        p.show(0, 1, { view, enc, fmt });
        if (p.box.readOnly) continue;
        // In Binary view the edit is a byte, 0x41, typed as hex; elsewhere the letter Z.
        const add = view === 'binary' ? '41' : 'Z';
        p.edit(where === 'start' ? 0 : p.box.value.length, 0, add);
        let want;
        if (kind === 'text') { const c = view === 'binary' ? 'A' : 'Z'; want = where === 'start' ? c + (v ?? '') : (v ?? '') + c; }
        else {
          const orig = p.f.hexToBytes(v || '0x'), b = view === 'binary' ? Uint8Array.of(0x41) : p.f.vpEncode('Z', enc).bytes;
          want = p.f.bytesToHex(where === 'start' ? concat(b, orig) : concat(orig, b));
        }
        const what = `${kind} ${JSON.stringify(v)} ${view} ${enc} fmt=${fmt} at the ${where}`;
        assert.equal(p.save(), true, what + ' ' + p.toasts.slice(-1));
        assert.deepEqual(p.saved, [want], what);
      }
    }
  }
});

test('an edit inside laid-out JSON keeps every number and line break it did not touch', { skip }, () => {
  const v = '{"id":12345678901234567890,\r\n "v":1.0, "s":"x"}';
  const p = panel({ cols: ['id', 'j'], rows: [['1', v]] });
  p.show(0, 1, { view: 'json', fmt: true });
  assert.equal(p.box.value, '{\n  "id": 12345678901234567890,\n  "v": 1.0,\n  "s": "x"\n}');
  const at = p.box.value.indexOf('1.0');
  p.edit(at, 3, '2.5');
  p.save();
  assert.deepEqual(p.saved, ['{"id":12345678901234567890,\r\n "v":2.5, "s":"x"}']);
});

test('what is typed in the layout\'s own whitespace is kept, and the layout is not', { skip }, () => {
  const p = panel({ cols: ['id', 'j'], rows: [['1', '{"a":1,"b":2}']] });
  p.show(0, 1, { view: 'json', fmt: true });
  p.edit(p.box.value.indexOf(',') + 1, 0, ' ');
  p.save();
  assert.deepEqual(p.saved, ['{"a":1, "b":2}']);
});

test('an edit inside laid-out XML changes only that text', { skip }, () => {
  const v = MARKUP[0][1];
  const p = panel({ cols: ['id', 'x'], rows: [['1', v]] });
  p.show(0, 1, { view: 'xml', fmt: true });
  p.edit(p.box.value.indexOf('>sp<') + 1, 2, 'SP');
  p.save();
  assert.deepEqual(p.saved, [v.replace('  sp  ', '  SP  ')]);
});

test('a binary value edited in the hex dump changes only that byte', { skip }, () => {
  const v = '0x' + '00112233445566778899aabbccddeeff'.repeat(3);
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', v]] });
  p.show(0, 1, { view: 'binary', fmt: true });
  assert.equal(p.box.value.split('\n').length, 3);
  assert.equal(p.$('vpg_t1').textContent, '00000000\n00000010\n00000020');
  p.edit(p.box.value.indexOf('ff'), 2, '7f');
  p.save();
  assert.deepEqual(p.saved, [v.replace('ff', '7f')]);
});

test('an edit carried from one view to another is saved as typed', { skip }, () => {
  const p = panel({ cols: ['id', 'j'], rows: [['1', '{"a":"x"}\r\n']] });
  p.show(0, 1, { view: 'json', fmt: true });
  p.edit(p.box.value.indexOf('"x"') + 2, 0, 'y');
  p.f.vpSet('t1', 'view', 'text');
  assert.equal(p.f.vpDirty('t1'), true);
  p.save();
  assert.deepEqual(p.saved, ['{"a":"xy"}\r\n']);
});

test('the bytes of a binary column, edited as UTF-16 text', { skip }, () => {
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', '0x48006900']] });
  p.show(0, 1, { view: 'text', enc: 'utf-16le' });
  assert.equal(p.box.value, 'Hi');
  p.edit(2, 0, '!');
  p.save();
  assert.deepEqual(p.saved, ['0x480069002100']);
});

test('a character the encoding cannot hold is refused, and nothing is saved', { skip }, () => {
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', '0x41']] });
  p.show(0, 1, { view: 'text', enc: 'latin1' });
  p.edit(1, 0, '€');
  assert.equal(p.save(), false);
  assert.deepEqual(p.saved, []);
  assert.ok(p.toasts.some(m => /U\+20AC.*Latin-1/.test(m)), p.toasts.join());
  assert.equal(p.f.vpDirty('t1'), true, 'the edit is still there to fix');
});

test('hex pasted into the text view of a binary column is refused', { skip }, () => {
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', '0x41']] });
  p.show(0, 1, { view: 'text', enc: 'utf-8' });
  p.type('0x0011223344556677');
  assert.equal(p.save(), false);
  assert.deepEqual(p.saved, []);
});

test('bytes that are not valid in the encoding are shown, but cannot be edited', { skip }, () => {
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', '0x41ff']] });
  p.show(0, 1, { view: 'text', enc: 'utf-8' });
  assert.equal(p.box.readOnly, true);
  assert.match(p.$('vpn_t1').textContent, /not valid UTF-8/);
  p.show(0, 1, { view: 'text', enc: 'shift_jis' });
  assert.equal(p.box.readOnly, true, 'nor in an encoding that can only be read');
});

test('a text column whose bytes were not UTF-8: shown in the encoding, saved as the characters', { skip }, () => {
  const p = panel({ cols: ['id', 'l'], rows: [['1', '0xe9ff']] });
  p.show(0, 1, { view: 'text', enc: 'windows-1252' });
  assert.equal(p.box.value, 'éÿ');
  assert.equal(p.save(), true); assert.deepEqual(p.saved, [], 'untouched, nothing is written');
  p.edit(2, 0, 'Z');
  p.save();
  assert.deepEqual(p.saved, ['éÿZ']);
  p.t.pending.upd = {};
  p.show(0, 1, { view: 'binary' });
  assert.equal(p.box.readOnly, true, 'its bytes cannot be saved as bytes');
});

test('NULL stays NULL unless something is typed', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', null]] });
  p.show(0, 1, { view: 'text' });
  assert.equal(p.box.placeholder, '(NULL)');
  p.edit(0, 0, 'x'); p.edit(0, 1, '');
  assert.equal(p.f.vpDirty('t1'), false);
  assert.equal(p.save(), true); assert.deepEqual(p.saved, []);
});

test('typing a change and taking it back again leaves nothing to save', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', '{"a":1}']] });
  p.show(0, 1, { view: 'json', fmt: true });
  // Typed into the layout's indentation, which drops what it had there - until it is typed back.
  p.edit(3, 0, 'q'); assert.equal(p.f.vpDirty('t1'), true);
  p.edit(3, 1, ''); assert.equal(p.f.vpDirty('t1'), false);
  p.edit(p.box.value.indexOf('1'), 1, '2');
  p.save();
  assert.deepEqual(p.saved, ['{"a":2}'], 'so an edit after it still saves only itself');
});

test('Revert throws the edit away', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', 'abc']] });
  p.show(0, 1, { view: 'text' });
  p.edit(3, 0, 'd'); p.f.vpRevert('t1');
  assert.equal(p.box.value, 'abc'); assert.equal(p.f.vpDirty('t1'), false);
});

// --- Moving on with an edit not saved -----------------------------------------------------------
test('clicking another cell with an edit not saved asks first, and each answer does what it says', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', 'a'], ['2', 'b']] });
  p.show(0, 1, { view: 'text' });
  p.edit(1, 0, 'X');
  p.f.vpShow('t1', { ri: 1, ci: 1 });
  assert.deepEqual(p.st().src, { ri: 0, ci: 1 }, 'still on the edited cell');
  assert.equal(p.$('vpa_t1').style.display, 'flex', 'and asking');
  p.f.vpAskDo('t1', 'stay');
  assert.equal(p.box.value, 'aX'); assert.deepEqual(p.saved, []);

  p.f.vpShow('t1', { ri: 1, ci: 1 });
  p.f.vpAskDo('t1', 'discard');
  assert.deepEqual(p.st().src, { ri: 1, ci: 1 }); assert.equal(p.box.value, 'b'); assert.deepEqual(p.saved, []);

  p.edit(1, 0, 'Y');
  p.f.vpShow('t1', { ri: 0, ci: 1 });
  p.f.vpAskDo('t1', 'save');
  assert.deepEqual(p.saved, ['bY']); assert.deepEqual(p.t.pending.upd, { '1:1': 'bY' });
  assert.deepEqual(p.st().src, { ri: 0, ci: 1 }); assert.equal(p.box.value, 'a');
});

test('an edit the cell cannot take keeps the panel on it, even when asked to save and move on', { skip }, () => {
  const p = panel({ cols: ['id', 'b'], binCols: [false, true], rows: [['1', '0x41'], ['2', '0x42']] });
  p.show(0, 1, { view: 'binary' });
  p.edit(0, 0, 'zz');
  p.f.vpShow('t1', { ri: 1, ci: 1 });
  p.f.vpAskDo('t1', 'save');
  assert.deepEqual(p.saved, []);
  assert.deepEqual(p.st().src, { ri: 0, ci: 1 });
  assert.ok(p.toasts.some(m => /not whole bytes in hex/.test(m)), p.toasts.join());
});

test('a new result drops the panel\'s value, and says so if an edit is lost', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [['1', 'a']] });
  p.show(0, 1, { view: 'text' });
  p.edit(1, 0, 'X');
  p.t.cols = ['id', 't']; p.f.vpRefresh('t1');
  assert.equal(p.st().src, undefined);
  assert.ok(p.toasts.some(m => /was not saved/.test(m)), p.toasts.join());
});

// --- What cannot be edited ----------------------------------------------------------------------
test('a result without a key, a read-only connection and a generated column are read-only', { skip }, () => {
  for (const o of [{ editable: false }, { readOnly: true }, { gen: ['t'] }]) {
    const p = panel({ cols: ['id', 't'], rows: [['1', 'a']], ...o });
    p.show(0, 1, { view: 'text' });
    assert.equal(p.box.readOnly, true, JSON.stringify(o));
    assert.equal(p.save(), false, JSON.stringify(o));
  }
});

test('a row added and not applied yet is edited in place', { skip }, () => {
  const p = panel({ cols: ['id', 't'], rows: [] });
  const row = { id: '9', t: 'new' };
  p.t.pending.ins.push(row);
  p.f.vpShow('t1', { row, col: 't' });
  assert.equal(p.box.value, 'new');
  p.edit(3, 0, '!'); p.save();
  assert.equal(row.t, 'new!');
});

// --- Files ------------------------------------------------------------------------------------
test('Save as file writes the bytes, or the text in the encoding with its own line breaks', { skip }, async () => {
  const p = panel({ cols: ['id', 'b', 't'], binCols: [false, true, false], rows: [['1', '0x89504e470d0a1a0a00', 'a\r\nb€']] });
  p.show(0, 1, { view: 'binary' });
  await p.f.vpToFile('t1');
  assert.equal(p.files[0].name, 't_b.png', 'an image is named as one');
  assert.deepEqual([...p.files[0].bytes], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0]);
  p.show(0, 2, { view: 'text', enc: 'windows-1252' });
  await p.f.vpToFile('t1');
  assert.equal(p.files[1].name, 't_t.txt');
  assert.deepEqual([...p.files[1].bytes], [0x61, 0x0d, 0x0a, 0x62, 0x80]);
  p.show(0, 2, { view: 'text', enc: 'shift_jis' });
  await p.f.vpToFile('t1');
  assert.equal(p.files.length, 2, 'not in an encoding that cannot be written');
});
