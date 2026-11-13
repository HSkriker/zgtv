(function () {
  const TV = (window.TV = window.TV || {});
  let el = null;
  let inited = false;

  function fmtPrice(v) {
    try {
      return TV.chart.candle.priceFormatter().format(v);
    } catch (_) {
      return Number(v).toFixed(2);
    }
  }

  function close() {
    if (!el) return false;
    el.remove();
    el = null;
    return true;
  }

  function open(x, y, items) {
    close();
    el = document.createElement('div');
    el.id = 'ctx-menu';
    el.className = 'ctx-menu';
    el.setAttribute('role', 'menu');

    for (const item of items) {
      const btn = document.createElement('button');
      btn.className = 'ctx-item' + (item.disabled ? ' disabled' : '');
      btn.textContent = item.label;
      btn.type = 'button';
      if (!item.disabled) {
        btn.addEventListener('click', () => {
          close();
          item.run();
        });
      }
      el.appendChild(btn);
    }

    document.body.appendChild(el);
    const r = el.getBoundingClientRect();
    el.style.left = Math.max(4, Math.min(x, window.innerWidth - r.width - 4)) + 'px';
    el.style.top = Math.max(4, Math.min(y, window.innerHeight - r.height - 4)) + 'px';
  }

  function fallbackCopy(text) {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } catch (_) {}
    ta.remove();
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
    } else {
      fallbackCopy(text);
    }
  }

  function init() {
    if (inited) return;
    inited = true;

    const host = document.getElementById('chart');
    host.addEventListener('contextmenu', ev => {
      ev.preventDefault();
      const panes = TV.chart.instance.panes();
      const pane = panes[0] && panes[0].getHTMLElement();
      if (!pane) return;
      const rect = pane.getBoundingClientRect();
      const x = ev.clientX - rect.left;
      const y = ev.clientY - rect.top;
      if (x < 0 || x > rect.width || y < 0 || y > rect.height) return;

      const raw = TV.chart.candle.coordinateToPrice(y);
      const price = raw != null && Number.isFinite(raw) ? raw : null;
      const hit = TV.draw && TV.draw.hitRowAt ? TV.draw.hitRowAt(x, y) : null;

      const items = [];
      if (price != null) {
        const text = fmtPrice(price);
        items.push({ label: `在此添加预警 ${text}`, run: () => TV.alerts.add(price) });
        items.push({ label: `复制价格 ${text}`, run: () => copyText(text) });
      }
      items.push({
        label: hit ? `删除「${TV.draw.labelOf(hit)}」` : '删除此画线（未命中）',
        disabled: !hit,
        run: () => { if (hit) TV.draw.removeRow(hit); },
      });
      open(ev.clientX, ev.clientY, items);
    });

    window.addEventListener('mousedown', ev => {
      if (el && !el.contains(ev.target)) close();
    }, true);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    document.addEventListener('scroll', close, true);
  }

  TV.menu = { init, close, open };
})();
