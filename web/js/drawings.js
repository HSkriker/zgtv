(function () {
  const TV = (window.TV = window.TV || {});
  const state = { mode: 'none', rows: [], lines: [], anchor: null, measure: null };
  let prim = null;
  let drawSeq = 0; // 竞态防护：切换标的/周期时，旧画线不得画到新图
  let hover = null;
  let drag = null;
  let suppressClick = false;

  function fmtTime(t) {
    if (t == null) return '-';
    if (typeof t === 'number') {
      const d = new Date(t * 1000);
      const p = n => String(n).padStart(2, '0');
      return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} `
        + `${p(d.getHours())}:${p(d.getMinutes())}`;
    }
    if (t.year) {
      return `${t.year}-${String(t.month).padStart(2, '0')}-${String(t.day).padStart(2, '0')}`;
    }
    return String(t);
  }

  function clearLines() {
    for (const l of state.lines) {
      try { TV.chart.candle.removePriceLine(l.api); } catch (_) {}
    }
    state.lines = [];
  }

  function addLineOnly(row) {
    const api = TV.chart.candle.createPriceLine({
      price: row.price, color: '#f0b90b', lineWidth: 1, lineStyle: 2,
      axisLabelVisible: true, title: 'H',
    });
    state.lines.push({ row, api });
  }

  function rebuild() {
    clearLines();
    const shapes = [];
    for (const r of state.rows) {
      if (r.kind === 'hline') addLineOnly(r);
      else shapes.push(r);
    }
    if (prim) prim.setData(shapes);
    renderList();
  }

  function save() {
    const s = TV.app.state;
    // 立刻写，不做防抖：单次只有几 KB，且避免切标的后写错 key
    TV.api.saveDrawings(s.storeKey || s.code, s.period, state.rows)
      .catch(e => console.error('画线保存失败', e));
  }

  function addRow(row) {
    state.rows.push(row);
    rebuild();
    save();
  }

  function removeAt(i) {
    if (i < 0 || i >= state.rows.length) return;
    state.rows.splice(i, 1);
    rebuild();
    save();
  }

  function priceAt(y) {
    const p = TV.chart.candle.coordinateToPrice(y);
    return p == null ? null : Number(p.toFixed(3));
  }

  function onClick(param) {
    if (suppressClick) { suppressClick = false; return; }
    if (state.mode === 'none' || !param.point) return;
    if (param.time == null) return;

    if (state.mode === 'hline') {
      const price = priceAt(param.point.y);
      if (price != null) addRow({ kind: 'hline', price });
      return;
    }

    if (state.mode === 'vline') {
      addRow({ kind: 'vline', t: param.time });
      return;
    }

    if (state.mode === 'text' || state.mode === 'pline') {
      const price = priceAt(param.point.y);
      if (price == null) return;
      if (state.mode === 'text') {
        const inp = document.getElementById('draw-text');
        const text = inp && inp.value.trim();
        addRow({ kind: 'text', t: param.time, p: price, text: text || '注' });
        if (inp) inp.value = '';
      } else {
        addRow({ kind: 'pline', t: param.time, p: price });
      }
      return;
    }

    if (state.mode === 'trend' || state.mode === 'rect' || state.mode === 'fib'
        || state.mode === 'measure') {
      const price = priceAt(param.point.y);
      if (price == null) return;
      if (!state.anchor) {
        if (state.mode === 'measure' && state.measure) {
          state.measure = null;
          prim.setMeasure(null);
        }
        state.anchor = { t: param.time, p: price };
        prim.setPending(state.anchor);
        renderPanel();
        return;
      }
      if (state.mode === 'measure') {
        state.measure = {
          t1: state.anchor.t, p1: state.anchor.p,
          t2: param.time, p2: price,
        };
        prim.setMeasure(state.measure);
      } else {
        addRow({
          kind: state.mode === 'rect' ? 'rect' : state.mode === 'fib' ? 'fib' : 'line',
          t1: state.anchor.t, p1: state.anchor.p,
          t2: param.time, p2: price,
        });
      }
      state.anchor = null;
      prim.setPending(null);
      renderPanel();
    }
  }

  function setMode(mode) {
    const next = state.mode === mode ? 'none' : mode;
    if (next !== 'measure' && state.measure) {
      state.measure = null;
      if (prim) prim.setMeasure(null);
    }
    state.mode = next;
    state.anchor = null;
    if (prim) prim.setPending(null);
    document.getElementById('chart').style.cursor = state.mode === 'none' ? '' : 'crosshair';
    renderPanel();
  }

  function label(r) {
    if (r.kind === 'hline') return `水平线 ${Number(r.price).toFixed(2)}`;
    if (r.kind === 'line') return `趋势线 ${Number(r.p1).toFixed(2)} → ${Number(r.p2).toFixed(2)}`;
    if (r.kind === 'rect') return `矩形 ${Number(r.p1).toFixed(2)} ~ ${Number(r.p2).toFixed(2)}`;
    if (r.kind === 'fib') return `斐波 ${Number(r.p1).toFixed(2)} ~ ${Number(r.p2).toFixed(2)}`;
    if (r.kind === 'vline') return `垂直线 ${fmtTime(r.t)}`;
    if (r.kind === 'text') return `文本 ${r.text}`;
    if (r.kind === 'pline') return `价格线 ${Number(r.p).toFixed(2)}`;
    return r.kind;
  }

  function renderList() {
    const el = document.getElementById('draw-list');
    if (!el) return;
    if (!state.rows.length) { el.innerHTML = '<div class="dim">暂无画线</div>'; return; }
    el.innerHTML = state.rows.map((r, i) => `
      <div class="watch-row">
        <span>${label(r)}</span>
        <button class="mini" data-del="${i}">×</button>
      </div>`).join('');
    el.querySelectorAll('button[data-del]').forEach(btn => {
      btn.addEventListener('click', () => removeAt(Number(btn.dataset.del)));
    });
  }

  function renderPanel() {
    const body = TV.panels.section('draw', '画线',
      '<button class="mini" id="draw-clear">清空</button>');
    body.innerHTML = `
      <div class="draw-tools">
        <button class="mini ${state.mode === 'hline' ? 'on' : ''}" data-mode="hline">水平线</button>
        <button class="mini ${state.mode === 'vline' ? 'on' : ''}" data-mode="vline">垂直线</button>
        <button class="mini ${state.mode === 'trend' ? 'on' : ''}" data-mode="trend">趋势线</button>
        <button class="mini ${state.mode === 'rect' ? 'on' : ''}" data-mode="rect">矩形</button>
        <button class="mini ${state.mode === 'fib' ? 'on' : ''}" data-mode="fib">斐波</button>
        <button class="mini ${state.mode === 'pline' ? 'on' : ''}" data-mode="pline">价格线</button>
        <button class="mini ${state.mode === 'text' ? 'on' : ''}" data-mode="text">文本</button>
        <button class="mini ${state.mode === 'measure' ? 'on' : ''}" data-mode="measure">测量</button>
      </div>
      <input id="draw-text" class="draw-input" placeholder="文本内容（选「文本」后点图上位置）">
      <div class="dim" style="margin-bottom:4px">${hintText()}</div>
      <div id="draw-list"></div>`;
    body.querySelectorAll('button[data-mode]').forEach(btn => {
      btn.addEventListener('click', () => setMode(btn.dataset.mode));
    });
    document.getElementById('draw-clear').addEventListener('click', () => {
      state.rows = [];
      state.anchor = null;
      state.measure = null;
      if (prim) {
        prim.setPending(null);
        prim.setMeasure(null);
      }
      rebuild();
      save();
    });
    renderList();
  }

  function hintText() {
    if (state.mode === 'trend' || state.mode === 'rect' || state.mode === 'fib'
        || state.mode === 'measure') {
      const what = state.mode === 'measure' ? '测量' : '画线';
      return state.anchor ? `已选起点，再点终点（${what}）` : `点两下${what}：起点 → 终点`;
    }
    if (state.mode === 'text') return '输入文字后点图上位置';
    if (state.mode === 'hline') return '点图上位置放置水平线（可拖动）';
    if (state.mode === 'vline') return '点图上位置放置垂直线';
    if (state.mode === 'pline') return '点图上位置向右画价格线';
    return '选工具后在图上点击';
  }

  async function load(stateMain) {
    const my = ++drawSeq;
    state.rows = [];
    state.anchor = null;
    state.measure = null;
    if (prim) {
      prim.setPending(null);
      prim.setMeasure(null);
    }
    rebuild();
    renderPanel();

    try {
      const j = await TV.api.drawings(stateMain.storeKey || stateMain.code, stateMain.period);
      if (my !== drawSeq) return; // 已切到别的标的/周期：丢弃过期画线
      state.rows = Array.isArray(j.rows) ? j.rows : [];
    } catch (e) {
      console.error('画线读取失败', e);
      state.rows = [];
    }
    rebuild();
  }

  function paneY(clientY) {
    const el = TV.chart.instance.panes()[0].getHTMLElement();
    if (!el) return null;
    return clientY - el.getBoundingClientRect().top;
  }

  function nearestHline(y) {
    let best = null;
    for (const r of state.rows) {
      if (r.kind !== 'hline') continue;
      const cy = TV.chart.candle.priceToCoordinate(r.price);
      if (cy == null) continue;
      const d = Math.abs(cy - y);
      if (d <= 6 && (!best || d < best.d)) best = { row: r, d };
    }
    return best;
  }

  function bindDrag() {
    const host = document.getElementById('chart');

    host.addEventListener('mousedown', ev => {
      if (ev.button !== 0 || state.mode !== 'none') return;
      const y = paneY(ev.clientY);
      if (y == null) return;
      const hit = nearestHline(y);
      if (!hit) return;
      drag = { row: hit.row, moved: false };
      ev.preventDefault();
      ev.stopPropagation();
      host.style.cursor = 'ns-resize';
    }, true);

    window.addEventListener('mousemove', ev => {
      if (drag) {
        const y = paneY(ev.clientY);
        const price = y == null ? null : priceAt(y);
        if (price == null) return;
        drag.row.price = price;
        drag.moved = true;
        const entry = state.lines.find(l => l.row === drag.row);
        if (entry) entry.api.applyOptions({ price });
        return;
      }
      if (state.mode !== 'none') return;
      if (!host.contains(ev.target)) return;
      const y = paneY(ev.clientY);
      const hit = y == null ? null : nearestHline(y);
      host.style.cursor = hit ? 'ns-resize' : '';
    });

    window.addEventListener('mouseup', () => {
      if (!drag) return;
      const moved = drag.moved;
      drag = null;
      host.style.cursor = state.mode === 'none' ? '' : 'crosshair';
      if (moved) {
        suppressClick = true;
        rebuild();
        save();
        setTimeout(() => { suppressClick = false; }, 0);
      }
    });
  }

  function setHover(t) {
    if (hover) hover.setTime(t == null ? null : t);
  }

  function escape() {
    if (state.mode === 'none' && !state.anchor) return false;
    state.mode = 'none';
    state.anchor = null;
    if (prim) prim.setPending(null);
    const host = document.getElementById('chart');
    if (host) host.style.cursor = '';
    renderPanel();
    return true;
  }

  function distSeg(px, py, x1, y1, x2, y2) {
    const dx = x2 - x1;
    const dy = y2 - y1;
    if (dx === 0 && dy === 0) return Math.hypot(px - x1, py - y1);
    let t = ((px - x1) * dx + (py - y1) * dy) / (dx * dx + dy * dy);
    t = Math.max(0, Math.min(1, t));
    return Math.hypot(px - (x1 + t * dx), py - (y1 + t * dy));
  }

  function hitRowAt(x, y) {
    const ts = TV.chart.instance.timeScale();
    const series = TV.chart.candle;
    for (let i = state.rows.length - 1; i >= 0; i--) {
      const r = state.rows[i];
      if (r.kind === 'hline') {
        const cy = series.priceToCoordinate(r.price);
        if (cy != null && Math.abs(y - cy) <= 6) return r;
      } else if (r.kind === 'vline') {
        const cx = ts.timeToCoordinate(r.t);
        if (cx != null && Math.abs(x - cx) <= 6) return r;
      } else if (r.kind === 'pline') {
        const cx = ts.timeToCoordinate(r.t);
        const cy = series.priceToCoordinate(r.p);
        if (cx != null && cy != null && x >= cx - 6 && Math.abs(y - cy) <= 6) return r;
      } else if (r.kind === 'text') {
        const cx = ts.timeToCoordinate(r.t);
        const cy = series.priceToCoordinate(r.p);
        if (cx != null && cy != null && Math.abs(x - cx) <= 8 && Math.abs(y - cy) <= 8) return r;
      } else if (r.kind === 'line' || r.kind === 'rect' || r.kind === 'fib') {
        const x1 = ts.timeToCoordinate(r.t1);
        const y1 = series.priceToCoordinate(r.p1);
        const x2 = ts.timeToCoordinate(r.t2);
        const y2 = series.priceToCoordinate(r.p2);
        if (x1 == null || y1 == null || x2 == null || y2 == null) continue;

        if (r.kind === 'line') {
          if (distSeg(x, y, x1, y1, x2, y2) <= 6) return r;
        } else if (r.kind === 'rect') {
          const l = Math.min(x1, x2);
          const rt = Math.max(x1, x2);
          const t = Math.min(y1, y2);
          const b = Math.max(y1, y2);
          if (x >= l && x <= rt && y >= t && y <= b) return r;
          const nearX = x >= l - 6 && x <= rt + 6;
          const nearY = y >= t - 6 && y <= b + 6;
          if ((nearX && (Math.abs(y - t) <= 6 || Math.abs(y - b) <= 6))
            || (nearY && (Math.abs(x - l) <= 6 || Math.abs(x - rt) <= 6))) return r;
        } else {
          if (distSeg(x, y, x1, y1, x2, y2) <= 6) return r;
          for (const ratio of [0, 0.236, 0.382, 0.5, 0.618, 0.786, 1]) {
            const cy = series.priceToCoordinate(r.p2 + (r.p1 - r.p2) * ratio);
            if (cy != null && Math.abs(y - cy) <= 5) return r;
          }
        }
      }
    }
    return null;
  }

  function removeRow(row) {
    const i = state.rows.indexOf(row);
    if (i >= 0) removeAt(i);
  }

  function init() {
    prim = new TV.DrawPrimitive();
    TV.chart.candle.attachPrimitive(prim);
    hover = new TV.HoverPrimitive();
    TV.chart.candle.attachPrimitive(hover);
    TV.chart.instance.subscribeClick(onClick);
    bindDrag();
  }

  TV.draw = {
    init, load, state, setHover, escape,
    hitRowAt, removeRow, labelOf: label,
  };
})();
