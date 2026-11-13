// 指标图层：数值全部来自后端 /api/ind（ta-lib 0.8），前端只负责建系列 / 上色 / 持久化开关。
// 每个指标可带参数（齿轮 → 参数面板），参数透传给后端的 p=，缓存键与 stamp 都含参数。
(function () {
  const TV = (window.TV = window.TV || {});
  const KEY = 'tv.ind.on';
  const PKEY = 'tv.ind.params';

  // 参数定义：fields 顺序即 p= 的顺序；int 表示整数，step 可为小数步长
  const DEFS = {
    ma: {
      label: 'MA', title: '均线（主图叠加）',
      params: {
        fields: [
          { label: '周期1', def: 5, min: 1, max: 500 },
          { label: '周期2', def: 10, min: 1, max: 500 },
          { label: '周期3', def: 20, min: 1, max: 500 },
          { label: '周期4', def: 40, min: 1, max: 500 },
          { label: '周期5', def: 60, min: 1, max: 500 },
          { label: '周期6', def: 120, min: 1, max: 500 },
          { label: '周期7', def: 250, min: 1, max: 500 },
        ],
      },
    },
    // 对齐 zg 的 EMA 族（ema:[13,21,70,84]），与 MA 各自独立开关
    ema: {
      label: 'EMA', title: '指数移动平均（主图叠加）',
      params: {
        fields: [
          { label: '周期1', def: 13, min: 1, max: 500 },
          { label: '周期2', def: 21, min: 1, max: 500 },
          { label: '周期3', def: 70, min: 1, max: 500 },
          { label: '周期4', def: 84, min: 1, max: 500 },
        ],
      },
    },
    boll: {
      label: 'BOLL', title: '布林带 · 上下轨间填色（主图叠加）', lineWidth: 2,
      params: {
        fields: [
          { label: '周期', def: 20, min: 2, max: 500 },
          { label: '倍数', def: 2, min: 0.1, max: 10, step: 0.1 },
        ],
        required: true,   // 两个参数都不可留空（留空会让后端拿不到周期）
      },
    },
    macd: {
      label: 'MACD', title: 'MACD（副图）',
      params: {
        fields: [
          { label: '快线', def: 12, min: 1, max: 500 },
          { label: '慢线', def: 26, min: 2, max: 1000 },
          { label: '信号', def: 9, min: 1, max: 500 },
        ],
        rule: 'fast<slow',   // 快线必须小于慢线
        required: true,      // 三个参数缺一不可（只发 2 个后端会拒绝）
      },
    },
  };

  // 叠在主图（与 K 线共用价格轴，天然参与 autoscale）的指标
  const MAIN = new Set(['ma', 'ema', 'boll']);
  // 用柱状画的系列键：hist=MACD 柱
  const BAR = new Set(['hist']);

  const COLOR = {
    'boll-up': '#2dd4bf', 'boll-mid': '#ff9800', 'boll-low': '#2dd4bf',
    dif: '#f6c85f', dea: '#4ea8de',
  };
  // MA 的周期可改 → 键名不固定（ma9/ma21…），按序号取色；7 色对应 zg 的 SMA 族默认 7 条
  const MA_COLORS = ['#f6c85f', '#4ea8de', '#b06ab3', '#7f8a9e', '#e07a5f', '#6fcf97', '#d46a9f'];
  // EMA 4 条沿用 zg MA_STYLE 的 EMA 配色
  const EMA_COLORS = ['#f59e0b', '#ffae92', '#a78bfa', '#22c55e'];

  // 图例里每个系列的短名（TradingView 风格：指标名 + 各线数值）
  const SERIES_LABEL = {
    'boll-up': '上轨', 'boll-mid': '中轨', 'boll-low': '下轨',
    dif: 'DIF', dea: 'DEA', hist: 'MACD',
  };

  const DEFS_ORDER = ['ma', 'ema', 'boll', 'macd'];
  const esc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  const LINE = {
    lineWidth: 1,
    priceLineVisible: false,
    lastValueVisible: false,
    crosshairMarkerVisible: false,
  };

  const on = new Set(readOn());
  const drawn = new Map();   // name -> { pane, series: Map<key, ISeriesApi> }
  const cache = new Map();   // name -> { stamp, payload }
  const err = new Map();     // name -> 加载失败原因
  let band = null;           // 布林带填色原语，挂在 boll-up 系列上
  let stamp = '';
  let token = 0;
  let pop = null;            // 参数浮层 { el, name }

  function readOn() {
    try {
      const a = JSON.parse(localStorage.getItem(KEY) || '[]');
      return Array.isArray(a) ? a.filter(k => DEFS[k]) : [];
    } catch (_) {
      return [];
    }
  }

  function saveOn() {
    try { localStorage.setItem(KEY, JSON.stringify([...on])); } catch (_) {} // 隐私模式/配额满不抛
  }

  // ---- 参数（localStorage: { ma: [5,10,20,40,60,120,250], boll: [20,2], macd: [12,26,9] }）----

  function readParams() {
    try {
      const o = JSON.parse(localStorage.getItem(PKEY) || '{}');
      return (o && typeof o === 'object') ? o : {};
    } catch (_) {
      return {};
    }
  }

  function writeParams(o) {
    try { localStorage.setItem(PKEY, JSON.stringify(o)); } catch (_) {} // 隐私模式/配额满不抛
  }

  function numOk(v, f) {
    if (!Number.isFinite(v)) return false;
    if (v < f.min || v > f.max) return false;
    if (f.step === undefined && !Number.isInteger(v)) return false;
    return true;
  }

  function clampNum(v, f) {
    let x = Number(v);
    if (!Number.isFinite(x)) x = f.def;
    x = Math.min(f.max, Math.max(f.min, x));
    if (f.step === undefined) x = Math.round(x);
    return x;
  }

  // 空值 = 这一条线不画（后端少给一个参数就少出一条系列）
  function normVals(name, values) {
    const fields = DEFS[name].params.fields;
    const out = [];
    for (let i = 0; i < fields.length; i++) {
      const v = values[i];
      if (v === null || v === undefined || v === '') { out.push(null); continue; }
      const x = Number(v);
      out.push(Number.isFinite(x) ? clampNum(x, fields[i]) : NaN);
    }
    return out;
  }

  // 当前生效的参数值（缺省 / 越界 → 回落到默认值；null = 该线不画）
  function paramsOf(name) {
    const def = DEFS[name];
    if (!def || !def.params) return [];
    const saved = readParams()[name];
    const fields = def.params.fields;
    if (!Array.isArray(saved) || saved.length !== fields.length) {
      return fields.map(f => f.def);
    }
    const vals = saved.map((v, i) => (v === null ? null : clampNum(Number(v), fields[i])));
    // 持久化里残留的非法值（如 MACD 曾存成 [12,26,null]）不能让整块指标一直报错
    if (def.params.required && vals.some(v => v === null)) return fields.map(f => f.def);
    if (!vals.some(v => v !== null)) return fields.map(f => f.def);
    return vals;
  }

  // 透给后端的 p= 串（空值不参与）
  function paramString(name) {
    return paramsOf(name).filter(v => v !== null).join(',');
  }

  // 参数变了要重新取数：清缓存 + 撤图层（否则会留下旧周期的孤儿系列）
  function setParams(name, values) {
    const def = DEFS[name];
    if (!def || !def.params) return false;
    if (!Array.isArray(values) || values.length !== def.params.fields.length) return false;
    const vals = normVals(name, values);
    if (vals.some(v => Number.isNaN(v))) return false;
    if (!vals.some(v => v !== null)) return false;
    if (def.params.required && vals.some(v => v === null)) return false;
    if (def.params.rule === 'fast<slow' && vals[0] !== null && vals[1] !== null
      && !(vals[0] < vals[1])) return false;
    const o = readParams();
    o[name] = vals;
    writeParams(o);
    cache.delete(name);
    drop(name);
    const st = TV.app && TV.app.state;
    if (on.has(name) && st) {
      setData(st, false).catch(e => console.error(e));
    } else {
      paintButtons();
      renderLegend();
    }
    return true;
  }

  function palette() {
    const us = TV.app && TV.app.state && TV.app.state.colorMode === 'us';
    return us ? { up: '#26a69a', down: '#ef5350' } : { up: '#ef5350', down: '#26a69a' };
  }

  function colorFor(name, key, index) {
    if (name === 'ma') return MA_COLORS[index % MA_COLORS.length];
    if (name === 'ema') return EMA_COLORS[index % EMA_COLORS.length];
    return COLOR[key] || '#9aa0aa';
  }

  function labelFor(name, key) {
    if (SERIES_LABEL[key]) return SERIES_LABEL[key];
    if (name === 'ma' || name === 'ema') return key.toUpperCase();
    return key;
  }

  function entry(name) {
    let e = drawn.get(name);
    if (e) return e;
    e = { pane: null, series: new Map() };
    if (!MAIN.has(name)) e.pane = TV.chart.addPane('ind:' + name);
    drawn.set(name, e);
    return e;
  }

  function seriesFor(name, key, color) {
    const e = entry(name);
    const hit = e.series.get(key);
    if (hit) return hit;
    const opts = { ...LINE, color: color || '#9aa0aa' };
    const lw = DEFS[name].lineWidth;
    if (lw) opts.lineWidth = lw;
    const type = BAR.has(key) ? LightweightCharts.HistogramSeries : LightweightCharts.LineSeries;
    if (BAR.has(key)) {
      opts.priceFormat = key === 'hist'
        ? { type: 'price', precision: 4, minMove: 0.0001 }
        : { type: 'volume' };
    }
    const s = e.pane
      ? e.pane.addSeries(type, opts)
      : TV.chart.instance.addSeries(type, opts, 0);
    e.series.set(key, s);
    return s;
  }

  function barColor(key, y, c) {
    return y >= 0 ? c.up : c.down;
  }

  function toPoints(values) {
    const out = new Array(values.length);
    for (let i = 0; i < values.length; i++) {
      out[i] = { time: values[i][0], value: values[i][1] };
    }
    return out;
  }

  // 指标图例：TradingView 风格，画布左上角浮层，每个指标一行、各线取光标处数值
  function fmtVal(key, v) {
    if (!Number.isFinite(v)) return '—';
    if (key === 'hist') return v.toFixed(4);
    return v.toFixed(2);
  }

  function seriesValue(s, key, seriesData) {
    if (seriesData) {
      const hit = seriesData.get(s);
      if (hit && Number.isFinite(hit.value)) return hit.value;
      return null;
    }
    const d = s.data();
    if (!d || !d.length) return null;
    const last = d[d.length - 1];
    return Number.isFinite(last.value) ? last.value : null;
  }

  function renderLegend(seriesData) {
    const el = document.getElementById('legend-ind');
    if (!el) return;
    const lines = [];
    for (const name of DEFS_ORDER) {
      if (!on.has(name)) continue;
      const e = drawn.get(name);
      if (!e) continue;
      const parts = [];
      let i = 0;
      for (const [key, s] of e.series) {
        const v = seriesValue(s, key, seriesData);
        if (v != null) {
          parts.push(`<span style="color:${colorFor(name, key, i)}">${esc(labelFor(name, key))} ${fmtVal(key, v)}</span>`);
        }
        i++;
      }
      if (!parts.length) continue;
      const ps = DEFS[name].params ? `(${paramString(name)})` : '';
      lines.push(`<div><b>${esc(DEFS[name].label)}${ps}</b>${parts.join(' ')}</div>`);
    }
    el.innerHTML = lines.join('');
  }

  function draw(name, payload) {
    let i = 0;
    for (const s of payload.series) {
      const color = colorFor(name, s.key, i);
      const ser = seriesFor(name, s.key, color);
      if (BAR.has(s.key)) {
        const c = palette();
        ser.setData(s.values.map(([t, v]) => {
          return { time: t, value: v, color: barColor(s.key, v, c) };
        }));
      } else {
        ser.setData(toPoints(s.values));
      }
      i++;
    }
    if (name === 'boll') paintBand(payload.series);
    renderLegend();
  }

  // 上下轨等长（BBANDS 三条一起出），逐点配对成填色多边形
  function paintBand(list) {
    const e = drawn.get('boll');
    const up = (list.find(s => s.key === 'boll-up') || {}).values || [];
    const low = (list.find(s => s.key === 'boll-low') || {}).values || [];
    if (!e || !TV.BandPrimitive) return;
    if (!up.length) { if (band) band.setData([]); return; } // 新数据无带时清残影，别留旧多边形
    const n = Math.min(up.length, low.length);
    const rows = new Array(n);
    for (let i = 0; i < n; i++) rows[i] = { t: up[i][0], up: up[i][1], low: low[i][1] };
    const ser = e.series.get('boll-up');
    if (!band) {
      band = new TV.BandPrimitive();
      ser.attachPrimitive(band);
    }
    band.setData(rows);
  }

  function drop(name) {
    const e = drawn.get(name);
    if (!e) return;
    for (const s of e.series.values()) {
      try { TV.chart.instance.removeSeries(s); } catch (_) { /* 已随窗销毁 */ }
    }
    if (name === 'boll') band = null;
    drawn.delete(name);
    if (e.pane) TV.chart.removeExtraPane('ind:' + name);
    renderLegend();
  }

  // 标的 / 周期 / 参数 任一变化都要换 stamp（缓存失效的钥匙，只写一处）
  function stampOf(st) {
    const side = st.kind === 'sector' ? 'b' : 's';
    const lmt = TV.time.lmt(st.period);
    const ps = DEFS_ORDER.filter(n => DEFS[n].params)
      .map(n => `${n}=${paramString(n)}`).join(',');
    return `${side}|${st.code}|${st.period}|${lmt}|${ps}`;
  }

  function repaint() {
    const st = TV.app && TV.app.state;
    if (!st) return;
    const cur = stampOf(st);
    for (const [name, c] of cache) {
      if (c.stamp === cur && drawn.has(name)) draw(name, c.payload);
    }
  }

  async function setData(st, fresh = false) {
    const isBoard = st.kind === 'sector';
    const lmt = TV.time.lmt(st.period);
    stamp = stampOf(st);
    const my = ++token;

    if (!on.size) {
      for (const name of [...drawn.keys()]) drop(name);
      cache.clear();
      err.clear();
      paintButtons();
      return;
    }

    await Promise.all([...on].map(async name => {
      let j;
      try {
        j = await TV.api.ind(st.code, st.period, lmt, name, isBoard, fresh, paramString(name));
      } catch (e) {
        console.error('指标加载失败', name, e);
        if (my === token) {
          drop(name);
          cache.delete(name);
          err.set(name, e.message || String(e));
          paintButtons();
        }
        return;
      }
      if (my !== token) return; // 期间又切了标的/周期，丢弃上一轮
      err.delete(name);
      cache.set(name, { stamp, payload: j });
      draw(name, j);
    }));

    // 关掉的指标要连窗一起撤
    for (const name of [...drawn.keys()]) {
      if (!on.has(name)) drop(name);
    }
    paintButtons();
  }

  function toggle(name) {
    if (!DEFS[name]) return;
    const st = TV.app && TV.app.state;
    if (on.has(name)) {
      on.delete(name);
      err.delete(name);
      drop(name);
    } else {
      on.add(name);
      err.delete(name);
      const c = cache.get(name);
      if (c && c.stamp === stamp) {
        draw(name, c.payload);
      } else {
        cache.delete(name);   // 缓存是别的标的/周期/参数的旧数据，丢掉好让下面重新拉
      }
    }
    saveOn();
    paintButtons();
    if (on.has(name) && st && !cache.has(name)) {
      setData(st, false).catch(e => console.error(e));
    }
  }

  function paintButtons() {
    for (const btn of document.querySelectorAll('[data-ind]')) {
      const name = btn.dataset.ind;
      const def = DEFS[name] || {};
      const bad = err.get(name);
      const ps = def.params ? ` ${paramString(name)}` : '';
      btn.classList.toggle('on', on.has(name) && !bad);
      btn.classList.toggle('err', !!bad);
      btn.title = bad ? `加载失败：${bad}` : `${def.title || ''}${ps}`;
    }
  }

  function resetScales() {
    for (const e of drawn.values()) {
      for (const s of e.series.values()) {
        s.priceScale().applyOptions({ autoScale: true });
      }
    }
  }

  // ---- 参数浮层 ----

  function closeParams() {
    TV.ui.close(); // onClosed 里置 pop = null
  }

  function openParams(name, anchor) {
    const def = DEFS[name];
    if (!def || !def.params) return;
    TV.ui.open({
      id: 'ind-params', anchor, anchorSel: '.ind-gear',
      render: el => { pop = { el, name }; buildParams(name, el); },
      onClosed: () => { pop = null; },
    });
  }

  function buildParams(name, el) {
    const def = DEFS[name];
    const fields = def.params.fields;
    const vals = paramsOf(name);
    el.className = 'ind-params';
    const rows = fields.map((f, i) => `
      <label class="ind-params-row">
        <span>${esc(f.label)}</span>
        <input type="number" data-idx="${i}" value="${vals[i] === null ? '' : vals[i]}"
          min="${f.min}" max="${f.max}"${f.step === undefined ? ' step="1"' : ` step="${f.step}"`}>
      </label>`).join('');
    el.innerHTML = `
      <div class="ind-params-title">${esc(def.label)} 参数</div>
      ${rows}
      <div class="ind-params-err" id="ind-params-err"></div>
      <div class="ind-params-btns">
        <button type="button" data-act="default">恢复默认</button>
        <button type="button" data-act="cancel">取消</button>
        <button type="button" data-act="ok" class="primary">应用</button>
      </div>`;

    const errEl = el.querySelector('#ind-params-err');
    const inputs = [...el.querySelectorAll('input[data-idx]')];

    const collect = () => inputs.map(i => (i.value.trim() === '' ? null : Number(i.value)));
    const validate = vals => {
      if (!vals.some(v => v !== null)) return '至少保留一条线';
      if (def.params.required) {
        const i = vals.findIndex(v => v === null);
        if (i >= 0) return `${fields[i].label}不能为空`;
      }
      for (let i = 0; i < fields.length; i++) {
        if (vals[i] === null) continue;
        if (!numOk(vals[i], fields[i])) {
          return `${fields[i].label} 需在 ${fields[i].min}~${fields[i].max} 之间${fields[i].step === undefined ? '的整数' : ''}`;
        }
      }
      if (def.params.rule === 'fast<slow' && vals[0] !== null && vals[1] !== null
        && !(vals[0] < vals[1])) {
        return '快线必须小于慢线';
      }
      return '';
    };

    el.querySelector('[data-act="ok"]').addEventListener('click', () => {
      const v = collect();
      const msg = validate(v);
      if (msg) { errEl.textContent = msg; return; }
      errEl.textContent = '';
      setParams(name, v);
      closeParams();
    });
    el.querySelector('[data-act="cancel"]').addEventListener('click', () => closeParams());
    el.querySelector('[data-act="default"]').addEventListener('click', () => {
      setParams(name, fields.map(f => f.def));
      closeParams();
    });
    for (const i of inputs) {
      i.addEventListener('keydown', e => {
        if (e.key === 'Enter') { e.preventDefault(); el.querySelector('[data-act="ok"]').click(); }
      });
      i.addEventListener('input', () => { errEl.textContent = ''; });
    }
    const first = inputs[0];
    if (first) { first.focus(); first.select(); }
  }

  function init() {
    for (const btn of document.querySelectorAll('[data-ind]')) {
      const name = btn.dataset.ind;
      btn.addEventListener('click', () => toggle(name));
      const def = DEFS[name];
      if (!def || !def.params) continue;
      // 齿轮：点它开参数面板（不触发指标开关）
      const gear = document.createElement('i');
      gear.className = 'ind-gear';
      gear.title = '参数';
      gear.textContent = '⚙';
      gear.addEventListener('click', e => {
        e.preventDefault();
        e.stopPropagation();
        openParams(name, gear);
      });
      btn.appendChild(gear);
    }
    paintButtons();
    // 十字光标移动时图例取光标处的指标值
    if (TV.chart.instance) {
      TV.chart.instance.subscribeCrosshairMove(param => {
        renderLegend(param && param.time ? param.seriesData : null);
      });
    }
  }

  TV.ind = {
    init,
    setData,
    toggle,
    repaint,
    resetScales,
    setParams,
    get params() { return readParams(); },
    get enabled() { return [...on]; },
    get seriesCount() {
      let n = 0;
      for (const e of drawn.values()) n += e.series.size;
      return n;
    },
    // 验收用：返回可序列化的结构 + 按名字取系列（供同一表达式内取 priceScale）
    debug() {
      const series = {};
      for (const [name, e] of drawn) {
        series[name] = {};
        for (const [k, s] of e.series) series[name][k] = s.data().length;
      }
      return {
        enabled: [...on], panes: TV.chart.extraPaneCount,
        band: band ? band.rows.length : 0, series,
        err: Object.fromEntries(err),
        params: readParams(),
        legend: (document.getElementById('legend-ind') || {}).textContent || '',
      };
    },
    get(name, key) {
      const e = drawn.get(name);
      if (!e) return null;
      return key ? e.series.get(key) : e.series.values().next().value;
    },
  };
})();
