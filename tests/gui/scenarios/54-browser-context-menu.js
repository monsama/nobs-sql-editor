// The browser's own context menu (Back, Refresh, Print, Inspect) does not show where the app has no
// menu of its own; in a text box, and on selected text, it still does, for Cut, Copy and Paste.
(async () => {
  const fire = el => { const e = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }); el.dispatchEvent(e); return e.defaultPrevented; };
  const sel = window.getSelection();
  sel.removeAllRanges();
  G.check('not on empty space', fire(document.body));
  G.check('nor on the tab bar', fire($('tabsbar')));
  const box = document.createElement('input');
  const area = document.createElement('textarea');
  const text = document.createElement('div');
  text.textContent = 'some text to select';
  document.body.append(box, area, text);
  try {
    G.check('but in a text box', !fire(box));
    G.check('and in a multi-line one, as the editor is', !fire(area));
    G.check('not on text that is not selected', fire(text));
    const r = document.createRange(); r.selectNodeContents(text); sel.addRange(r);
    G.check('but on selected text', !fire(text));
  } finally {
    sel.removeAllRanges();
    box.remove(); area.remove(); text.remove();
  }
  return G.report();
})()
