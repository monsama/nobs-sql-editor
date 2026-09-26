// Find and replace in the editor, as a user drives it: Ctrl+F and Ctrl+H open the bar, the count says
// where you are, Enter and F3 step through, Replace takes one and All takes the rest, a regex uses its
// groups, a regex that does not parse says so, and Ctrl+Z takes a replacement back.
(async () => {
  const id = openTab('find', "select a from t where a = 'x';\nSELECT b FROM u;\nselect c from v;", '', false);
  const ed = $('ed_' + id);
  const key = (el, o) => el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...o }));
  const type = (el, v) => { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); };
  const count = () => $('frn_' + id).textContent;
  const sel = () => ed.value.slice(ed.selectionStart, ed.selectionEnd);
  try {
    ed.focus(); ed.setSelectionRange(0, 0);
    key(ed, { key: 'f', ctrlKey: true });
    G.check('Ctrl+F opens the find bar, without the replace row', $('fr_' + id).style.display !== 'none' && $('frr_' + id).style.display === 'none' && document.activeElement === $('frq_' + id));
    type($('frq_' + id), 'select');
    G.eq('it finds every match, whatever the case', count(), '1 of 3');
    G.eq('and selects the first', [ed.selectionStart, sel()], [0, 'select']);
    key($('frq_' + id), { key: 'Enter' });
    G.eq('Enter goes to the next', [count(), sel()], ['2 of 3', 'SELECT']);
    key($('frq_' + id), { key: 'F3', shiftKey: true });
    G.eq('Shift+F3 back', count(), '1 of 3');
    key($('frq_' + id), { key: 'Enter', shiftKey: true });
    G.eq('and back again wraps round to the last', count(), '3 of 3');
    $('frc_' + id).click();
    G.eq('Match case leaves the two in lower case', count().replace(/^\d+ /, '? '), '? of 2');
    $('frc_' + id).click();
    G.eq('the marks under the text are one per match', $('fm_' + id).querySelectorAll('mark').length, 3);

    type($('frq_' + id), 'nothing like this');
    G.eq('no match says so', count(), 'no results');
    $('frx_' + id).click(); type($('frq_' + id), '(unclosed');
    G.eq('a regex that does not parse says so', count(), 'not a regex');

    // replace
    key($('frq_' + id), { key: 'h', ctrlKey: true });
    G.check('Ctrl+H shows the replace row', $('frr_' + id).style.display !== 'none');
    type($('frq_' + id), 'from (\\w)'); $('frw_' + id).value = 'FROM tbl_$1';
    ed.setSelectionRange(0, 0); findStep(id, 1);
    findReplace(id);
    G.check('Replace takes the match in view, with its group', ed.value.startsWith("select a FROM tbl_t where") && (ed.value.match(/FROM tbl_/g) || []).length === 1, ed.value);
    findReplaceAll(id);
    G.eq('All takes the rest', (ed.value.match(/FROM tbl_/g) || []).length, 3);
    G.check('leaving the rest of the text as it was', ed.value.includes("where a = 'x';") && ed.value.includes('SELECT b '), ed.value);
    document.execCommand('undo');
    G.eq('Ctrl+Z takes Replace all back in one step', (ed.value.match(/FROM tbl_/g) || []).length, 1);

    $('frx_' + id).click();
    type($('frq_' + id), "'x'"); $('frw_' + id).value = '$1 and $&';
    findReplaceAll(id);
    G.check('without regex a $ in the replacement is a $', ed.value.includes("where a = $1 and $&;"), ed.value);

    key($('frq_' + id), { key: 'Escape' });
    G.check('Esc closes the bar and its marks', $('fr_' + id).style.display === 'none' && $('fm_' + id).style.display === 'none');
    G.eq('the coloured text under the editor shows what was replaced', $('hl_' + id).textContent.replace(/\n$/, ''), ed.value);
  } finally {
    closeTab(id);
  }
  return G.report();
})()
