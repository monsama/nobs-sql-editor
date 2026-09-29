// A floating window keeps the size and place it was given - resized by its corner, dragged, closed
// maximized - across closing it and a restart, is moved back on screen when that place no longer
// fits, and Restore defaults in Settings forgets it all. One never resized keeps no size, so a
// default changed later still reaches it.
(async () => {
  const id = 'mHist', box = $(id).querySelector('.box'), d = window._floatingDefaultSize[id];
  const kept = () => dlgGeomAll()[id] || null;
  const size = () => [box.style.width, box.style.height];
  const before = localStorage.getItem('dlgGeom'), lightBefore = !document.body.classList.contains('dark');
  try {
    localStorage.removeItem('dlgGeom'); delete window._floatingPos[id];
    G.eq('History opens bigger than it did, and no taller than the window', [d.width, d.height, d.maxHeight], ['1200px', '80vh', '900px']);
    show(id); hide(id);
    G.eq('opened and closed untouched, it keeps nothing', kept(), null);

    show(id);
    box.style.width = '700px'; box.style.height = '450px';
    box.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    G.eq('a resize by the corner is kept when the button is let go', kept() && [kept().width, kept().height], ['700px', '450px']);
    const r = box.getBoundingClientRect();
    floatDragStart(new MouseEvent('mousedown', { clientX: r.left + 50, clientY: r.top + 10 }), id);
    floatDragMove(new MouseEvent('mousemove', { clientX: 90, clientY: 40 }));
    floatDragEnd();
    G.eq('and so is a drag', kept() && kept().pos, { top: 30, left: 40 });
    hide(id);
    G.eq('closed, the box itself is back at its default', size(), [d.width, d.height]);

    // A restart: nothing in memory, only what was stored.
    delete window._floatingPos[id];
    show(id);
    G.eq('opened again, after a restart too, at that size and place', [...size(), box.style.top, box.style.left], ['700px', '450px', '30px', '40px']);

    floatToggleMaximize(id); hide(id); show(id);
    G.eq('closed maximized, it opens at the size Restore goes back to', size(), ['700px', '450px']);

    window._floatingPos[id] = { top: window.innerHeight - 100, left: window.innerWidth - 100 };
    hide(id); delete window._floatingPos[id]; show(id);
    const o = box.getBoundingClientRect();
    G.check('a place that would leave it partly off screen is moved in', o.left >= 0 && o.top >= 0 && o.right <= window.innerWidth + 1 && o.bottom <= window.innerHeight + 1,
      { l: o.left, t: o.top, r: o.right, b: o.bottom, w: window.innerWidth, h: window.innerHeight });

    // Restore defaults, with the window open.
    await resetAppearance();
    G.eq('Restore defaults forgets every window\'s size and place', localStorage.getItem('dlgGeom'), null);
    G.eq('and the open one is back at its default size', size(), [d.width, d.height]);
    const c = box.getBoundingClientRect();
    G.check('and centred across', Math.abs((c.left + c.right) / 2 - window.innerWidth / 2) < 2, { l: c.left, r: c.right, w: window.innerWidth });
    hide(id);
    G.eq('closed after that, it keeps nothing', localStorage.getItem('dlgGeom'), null);
  } finally {
    hide(id); delete window._floatingPos[id];
    if (before === null) localStorage.removeItem('dlgGeom'); else localStorage.setItem('dlgGeom', before);
    if (lightBefore && document.body.classList.contains('dark')) toggleTheme();
  }
  return G.report();
})()
