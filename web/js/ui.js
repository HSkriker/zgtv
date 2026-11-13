// 顶栏齿轮/开关参数浮层公共件：同屏只保留一个（打开新的自动关旧的），
// Esc / 点浮层外关闭、锚点按钮上的点击不算外点。candles / pa2 / volume-profile / ind
// 四处曾各自复制同一套实现（含定位算法），收敛于此；调用方持有各自 render/onClosed。
(function () {
  const TV = (window.TV = window.TV || {});
  let active = null; // { el, anchorSel, render, onClosed }

  function close() {
    if (!active) return;
    const el = active.el;
    const onClosed = active.onClosed;
    active = null;
    document.removeEventListener('keydown', onKey, true);
    document.removeEventListener('mousedown', onOutside, true);
    if (el && el.parentNode) el.parentNode.removeChild(el);
    if (onClosed) onClosed();
  }

  function onKey(e) {
    if (e.key === 'Escape') { e.stopPropagation(); e.preventDefault(); close(); }
  }

  function onOutside(e) {
    if (!active) return;
    if (active.el.contains(e.target)) return;
    if (e.target.closest && e.target.closest(active.anchorSel)) return;
    close();
  }

  /** opts: { id, cls?, anchor, anchorSel, render(el), onClosed }。
   * render 在每次内容需要刷新时被调（打开时 + TV.ui.sync()），
   * 定位按锚点按钮下缘展开、贴边自动翻转。 */
  function open(opts) {
    close();
    const el = document.createElement('div');
    el.id = opts.id;
    el.className = 'ind-params' + (opts.cls ? ' ' + opts.cls : '');
    document.body.appendChild(el);
    active = { el, anchorSel: opts.anchorSel, render: opts.render, onClosed: opts.onClosed };
    if (opts.render) opts.render(el);

    const r = ((opts.anchor.closest && opts.anchor.closest('button')) || opts.anchor)
      .getBoundingClientRect();
    const w = el.offsetWidth, h = el.offsetHeight;
    let left = Math.min(r.left, window.innerWidth - w - 8);
    let top = r.bottom + 6;
    if (left < 8) left = 8;
    if (top + h > window.innerHeight - 8) top = Math.max(8, r.top - h - 6);
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;

    document.addEventListener('keydown', onKey, true);
    document.addEventListener('mousedown', onOutside, true);
  }

  /** 浮层开着时重渲染当前模块的内容（bars/开关变化后调用） */
  function sync() {
    if (active && active.render) active.render(active.el);
  }

  TV.ui = { open, close, sync, isOpen: () => !!active };
})();
