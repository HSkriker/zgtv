/* Volume Profile：价格分桶成交量分布。
   定义移植自 gs/zg 的 volume-profile-layer.js（算法参照 MQLTA MT5 Volume Profile）：
   - 价格按 1/2/5×10^k 步长分桶（目标 60 桶），每根K线的量在其 high-low 覆盖的桶间均分，
     保证 Σ桶量 == ΣK线量（体积守恒）；
   - POC = 最大量桶，Value Area 自 POC 向两侧逐桶取量大的一侧，累计 ≥70%；
   - A股涨跌停价区：按代码/ST 判涨跌幅，逐根判触板，触板价所在桶标黄/标绿；
   - 区间：可见区间 / 自适应（按波动率选窗口）/ 近N根 / 近1~5日（分钟）/ 自定义日期。
   渲染用 lightweight-charts 原生 primitive（canvas），右锚横向柱 + POC/VA 线，
   不依赖 ECharts，也不请求额外数据。 */
(function () {
  const TV = (window.TV = window.TV || {});

  const TARGET_BINS = 60;
  const VA_PCT = 0.70;
  const BAR_MAX_WIDTH_PCT = 0.22;
  const DEF_RANGE = '20';
  const MIN_DEFAULT = { 1: 'd1', 5: 'd1', 15: 'd2', 30: 'd3', 60: 'd5' };
  const DAY_MODES = ['d1', 'd2', 'd3', 'd5'];
  const RANGE_OPTIONS = [
    ['visible', '可见区间'],
    ['auto', '自适应（按波动率）'],
    ['d1', '近1日'], ['d2', '近2日'], ['d3', '近3日'], ['d5', '近5日'],
    ['20', '近20根（约1月）'], ['60', '近60根（约1季）'],
    ['120', '近120根（约半年）'], ['250', '近250根（约1年）'],
    ['custom', '自定义区间'],
  ];
  const AUTO_CANDS = [10, 15, 20, 30, 40, 60, 90, 120, 180, 250];
  const AUTO_MIN_DAYS = [1, 2, 3, 5, 8, 10];
  const AUTO_DIST_MIN = 3;
  const AUTO_DIST_MIN_MIN = 1.2;
  const AUTO_ATR_MULT = 2;
  const COL = {
    bar: 'rgba(59,130,246,0.30)',
    va: 'rgba(59,130,246,0.55)',
    poc: 'rgba(248,113,113,0.85)',
    zt: 'rgba(255,214,10,0.55)',
    dt: 'rgba(34,197,94,0.55)',
    pocLine: '#f87171',
    vaLine: '#94a3b8',
  };

  const isMinute = p => /^\d+$/.test(String(p || ''));

  function readPref(key, fallback) {
    try {
      const v = localStorage.getItem(key);
      return v == null ? fallback : v;
    } catch (_) {
      return fallback;
    }
  }

  function writePref(key, value) {
    try { localStorage.setItem(key, String(value)); } catch (_) {}
  }

  /* ---- A股涨跌幅（与 zg app-core 同序：板块/指数先排除，创业板科创板先于 ST） ---- */
  function limitPct(code, name) {
    const c = String(code || '');
    if (!/^\d{6}$/.test(c)) return null;
    if (/^(88|99|39)/.test(c)) return null;
    if (/^(300|301|302|688|689)/.test(c)) return 0.20;
    if (/^(43|83|87|92)/.test(c)) return 0.30;
    if (/^\*?ST/i.test(name || '')) return 0.05;
    return 0.10;
  }

  function pad2(n) { return String(n).padStart(2, '0'); }

  function dayKey(t) {
    if (typeof t === 'number') {
      const d = new Date((t + 8 * 3600) * 1000);
      return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;
    }
    if (t && typeof t === 'object' && t.year) {
      return `${t.year}-${pad2(t.month)}-${pad2(t.day)}`;
    }
    return String(t == null ? '' : t).slice(0, 10);
  }

  /* 触板价收集：日线用前一根收盘，分钟线用前一交易日收盘；周/月/季/年不判。 */
  function limitTouches(all, w0, w1, code, name, period) {
    const zt = [], dt = [];
    const pct = limitPct(code, name);
    if (pct == null) return { zt, dt };
    const p = String(period || 'day');
    const min = isMinute(p);
    if (!min && p !== 'day') return { zt, dt };
    let dayPrev = null;
    if (min) {
      const keys = [], close = new Map();
      for (const b of all) {
        const k = dayKey(b.time);
        if (!close.has(k)) keys.push(k);
        close.set(k, b.close);
      }
      dayPrev = new Map();
      for (let i = 1; i < keys.length; i++) dayPrev.set(keys[i], close.get(keys[i - 1]));
    }
    for (let i = Math.max(1, w0); i <= w1 && i < all.length; i++) {
      const b = all[i];
      if (!b) continue;
      const base = min ? dayPrev.get(dayKey(b.time)) : (all[i - 1] && all[i - 1].close);
      if (!(base > 0)) continue;
      const eps = Math.max(0.005, base * 0.001);
      const lu = Math.round(base * (1 + pct) * 100) / 100;
      const ld = Math.round(base * (1 - pct) * 100) / 100;
      if (b.high >= lu - eps) zt.push(lu);
      if (b.low <= ld + eps) dt.push(ld);
    }
    return { zt, dt };
  }

  function markBins(bins, ztPrices, dtPrices) {
    let nZt = 0, nDt = 0;
    for (const b of bins) {
      b.zt = ztPrices.some(p => p >= b.lo && p < b.hi) ? 1 : 0;
      b.dt = dtPrices.some(p => p >= b.lo && p < b.hi) ? 1 : 0;
      if (b.zt) nZt++;
      if (b.dt) nDt++;
    }
    return { nZt, nDt };
  }

  const fmtVol = TV.time.fmtVol; // 统一口径（time.js 定义，主图图例同用）

  /* 核心计算（纯函数）：bars 只需 high/low/vol。 */
  function compute(bars) {
    const valid = (bars || []).filter(b => b && Number.isFinite(b.high) && Number.isFinite(b.low)
      && b.high >= b.low && b.low > 0 && Number.isFinite(b.vol) && b.vol > 0);
    if (valid.length < 2) return { ok: false, reason: '有效K线不足' };
    let lo = Infinity, hi = -Infinity;
    for (const b of valid) {
      if (b.low < lo) lo = b.low;
      if (b.high > hi) hi = b.high;
    }
    const span = hi - lo;
    if (!(span > 0)) return { ok: false, reason: '价格区间为零' };

    const raw = span / TARGET_BINS;
    const mag = Math.pow(10, Math.floor(Math.log10(raw)));
    const norm = raw / mag;
    const step = (norm < 1.5 ? 1 : norm < 3.5 ? 2 : norm < 7.5 ? 5 : 10) * mag;
    const base = Math.floor(lo / step) * step;
    // +1e-9 抵消浮点误差，避免最高价被挤到桶外
    const bidx = p => Math.floor((p - base) / step + 1e-9);
    const nb = Math.max(1, bidx(hi) + 1);
    const vol = new Float64Array(nb);
    const cov = new Uint16Array(nb);
    for (const b of valid) {
      const i0 = Math.max(0, bidx(b.low));
      const i1 = Math.min(nb - 1, bidx(b.high));
      const w = b.vol / (i1 - i0 + 1);
      for (let i = i0; i <= i1; i++) { vol[i] += w; cov[i]++; }
    }

    let total = 0, poc = 0, maxVol = 0, covSum = 0, covMax = 0;
    for (let i = 0; i < nb; i++) {
      total += vol[i];
      if (vol[i] > vol[poc]) poc = i;
      if (vol[i] > maxVol) maxVol = vol[i];
      covSum += cov[i];
      if (cov[i] > covMax) covMax = cov[i];
    }
    if (!(total > 0) || !(maxVol > 0)) return { ok: false, reason: '总量为零' };

    let acc = vol[poc], up = poc, dn = poc;
    const vaFlag = new Uint8Array(nb);
    vaFlag[poc] = 1;
    while (acc < VA_PCT * total) {
      const u = up + 1 < nb ? vol[up + 1] : -1;
      const d = dn - 1 >= 0 ? vol[dn - 1] : -1;
      if (u < 0 && d < 0) break;
      if (u >= d) { up++; acc += vol[up]; vaFlag[up] = 1; }
      else { dn--; acc += vol[dn]; vaFlag[dn] = 1; }
    }

    const bins = [];
    for (let i = 0; i < nb; i++) bins.push({
      lo: base + i * step,
      hi: base + (i + 1) * step,
      vol: vol[i],
      frac: vol[i] / maxVol,
      poc: i === poc ? 1 : 0,
      va: vaFlag[i],
      zt: 0,
      dt: 0,
      cur: 0,
    });
    return {
      ok: true, bins, step, total, nBars: valid.length,
      poc: base + (poc + 0.5) * step,
      vah: base + (up + 1) * step,
      val: base + dn * step,
      vaPct: acc / total,
      coverAvg: covSum / nb,
      coverMax: covMax,
    };
  }

  function coverRange(all, start, end) {
    const s = String(start || '').trim();
    const e = String(end || '').trim();
    if (!s || !e || s > e) return null;
    let w0 = -1, w1 = -1;
    for (let i = 0; i < all.length; i++) {
      const d = dayKey(all[i].time);
      if (d >= s && d <= e) {
        if (w0 < 0) w0 = i;
        w1 = i;
      }
    }
    return { w0, w1 };
  }

  function modeBars(mode, period) {
    const days = mode === 'd1' ? 1 : mode === 'd2' ? 2 : mode === 'd3' ? 3 : mode === 'd5' ? 5 : 0;
    if (days) return isMinute(period) ? Math.max(2, Math.round(240 / (+period || 1)) * days) : days;
    return +mode || 20;
  }

  /* 现价量分位：下方（支撑侧）成交量占比；落在桶内按比例拆分。 */
  function pricePosition(bins, price) {
    let total = 0, below = 0, curBin = -1;
    for (let i = 0; i < bins.length; i++) {
      const b = bins[i];
      total += b.vol;
      if (price >= b.hi) below += b.vol;
      else if (price >= b.lo) {
        below += b.vol * (price - b.lo) / (b.hi - b.lo);
        if (curBin < 0) curBin = i;
      }
    }
    return { pct: total > 0 ? below / total * 100 : 0, curBin, total };
  }

  function atrPct(bars, n) {
    const w = (bars || []).slice(-(n || 60));
    if (w.length < 2) return NaN;
    let s = 0;
    for (let i = 1; i < w.length; i++) {
      const pc = w[i - 1].close;
      s += Math.max(w[i].high - w[i].low, Math.abs(w[i].high - pc), Math.abs(w[i].low - pc));
    }
    const last = w[w.length - 1].close;
    return last > 0 ? s / (w.length - 1) / last * 100 : NaN;
  }

  function autoCands(period) {
    if (!isMinute(period)) return { cands: AUTO_CANDS, tol: AUTO_DIST_MIN };
    const perDay = Math.max(2, Math.round(240 / (+period || 1)));
    return { cands: AUTO_MIN_DAYS.map(d => perDay * d), tol: AUTO_DIST_MIN_MIN };
  }

  /* 自适应：候选窗口里取「POC 距现价仍在容差内」的最大窗口；
     容差 = max(下限, 2×ATR60%)；都不满足时取最贴近现价者并标 far。 */
  function autoWindow(bars, price, period) {
    const n = (bars || []).length;
    const atr = atrPct(bars);
    const plan = autoCands(period);
    const tol = Math.max(plan.tol, Number.isFinite(atr) ? AUTO_ATR_MULT * atr : plan.tol);
    const cands = plan.cands.filter(L => L <= n);
    if (!cands.length) return { L: n, dist: NaN, cover: NaN, atr, tol, warn: 'no-data' };
    let best = null, closest = null;
    for (const L of cands) {
      const r = compute(bars.slice(n - L));
      if (!r.ok) continue;
      const dist = (price > 0 && Number.isFinite(r.poc))
        ? Math.abs(price - r.poc) / price * 100 : NaN;
      if (!Number.isFinite(dist)) continue;
      const item = { L, dist, cover: r.coverAvg, poc: r.poc };
      if (dist <= tol && (!best || L > best.L)) best = item;
      if (!closest || dist < closest.dist) closest = item;
    }
    const pick = best || closest;
    if (!pick) return { L: cands[0], dist: NaN, cover: NaN, atr, tol, warn: 'no-data' };
    return {
      L: pick.L, dist: pick.dist, cover: pick.cover, poc: pick.poc,
      atr, tol, warn: best ? '' : 'far',
    };
  }

  function effectiveMode(period, pref) {
    const min = isMinute(period);
    if (pref === 'custom') return 'custom';
    if (pref && RANGE_OPTIONS.some(o => o[0] === pref)) {
      if (!min && DAY_MODES.includes(pref)) return DEF_RANGE;
      return pref;
    }
    return min ? (MIN_DEFAULT[period] || 'd5') : DEF_RANGE;
  }

  /* ---- 状态 ---- */
  const prefs = {
    enabled: readPref('tv.vp.on', '1') !== '0',
    mode: readPref('tv.vp.range', ''),
    start: readPref('tv.vp.start', ''),
    end: readPref('tv.vp.end', ''),
  };
  let enabled = prefs.enabled;
  let prefMode = prefs.mode;
  let rangeMode = DEF_RANGE;
  let customStart = prefs.start;
  let customEnd = prefs.end;

  let prim = null;
  let pop = null;   // 挂在 VP 按钮齿轮下的浮层（与「形」按钮浮层同款）
  let all = [];
  let period = 'day';
  let code = '';
  let name = '';
  let slice = null;
  let result = null;
  let failReason = '';
  let lastAuto = null;
  let isBoard = false;

  function rangeLabel() {
    if (rangeMode === 'custom') return `自定义 ${customStart || '?'}~${customEnd || '?'}`;
    if (rangeMode === 'auto') {
      if (!lastAuto) return '自适应';
      if (isMinute(period)) {
        const perDay = Math.max(2, Math.round(240 / (+period || 1)));
        const days = lastAuto.L / perDay;
        return `自适应${days.toFixed(days % 1 ? 1 : 0)}日(${lastAuto.L}根)`;
      }
      return `自适应${lastAuto.L}根`;
    }
    const o = RANGE_OPTIONS.find(x => x[0] === rangeMode);
    return o ? o[1] : rangeMode;
  }

  function windowSlice() {
    const n = all.length;
    lastAuto = null;
    if (!n) return { reason: '无K线' };
    if (rangeMode === 'auto') {
      const price = all[n - 1].close;
      lastAuto = autoWindow(all, price, period);
      const w0 = Math.max(0, n - lastAuto.L);
      return { w0, w1: n - 1, bars: all.slice(w0) };
    }
    if (rangeMode === 'visible') {
      let lr = null;
      try { lr = TV.chart.instance.timeScale().getVisibleLogicalRange(); } catch (_) {}
      if (lr) {
        const w0 = Math.max(0, Math.floor(lr.from));
        const w1 = Math.min(n - 1, Math.ceil(lr.to));
        if (w0 <= w1) return { w0, w1, bars: all.slice(w0, w1 + 1) };
      }
      const w0 = Math.max(0, n - 20);
      return { w0, w1: n - 1, bars: all.slice(w0) };
    }
    if (rangeMode === 'custom') {
      const cr = coverRange(all, customStart, customEnd);
      if (!cr) return { reason: `起止日期无效（${customStart || '?'} ~ ${customEnd || '?'}，需起始 ≤ 结束）` };
      if (cr.w0 < 0) return { reason: `区间内无K线（${customStart || '?'} ~ ${customEnd || '?'}）` };
      return { w0: cr.w0, w1: cr.w1, bars: all.slice(cr.w0, cr.w1 + 1) };
    }
    const w0 = Math.max(0, n - modeBars(rangeMode, period));
    return { w0, w1: n - 1, bars: all.slice(w0) };
  }

  function updateStatus() {
    const el = document.getElementById('vpStatus');
    if (!el) return;
    if (!enabled) { el.textContent = '已关闭'; el.title = ''; return; }
    if (!result) {
      el.textContent = failReason ? 'VP：' + failReason : '无K线';
      el.title = '';
      return;
    }
    const r = result;
    let ztNote = '';
    if (r.ztN) ztNote += ` · 涨停区×${r.ztN}`;
    if (r.dtN) ztNote += ` · 跌停区×${r.dtN}`;
    let posTxt = '';
    const lines = [];
    if (r.curPrice != null && r.pos) {
      const dev = (r.curPrice - r.poc) / r.poc * 100;
      const sign = dev >= 0 ? '+' : '';
      const inVa = r.curPrice >= r.val && r.curPrice <= r.vah;
      posTxt = ` · 现价 ${r.curPrice.toFixed(2)} 距POC ${sign}${dev.toFixed(1)}% · 量分位 ${r.pos.pct.toFixed(0)}%`;
      lines.push(
        `现价 ${r.curPrice.toFixed(2)}（${inVa ? '在 VA 内' : r.curPrice > r.vah ? '在 VA 上方' : '在 VA 下方'}）`,
        `量分位 ${r.pos.pct.toFixed(1)}%：下方（支撑侧）${r.pos.pct.toFixed(1)}% 成交量，上方（阻力侧）${(100 - r.pos.pct).toFixed(1)}%`,
        `距 POC ${sign}${dev.toFixed(2)}%（POC ${r.poc.toFixed(2)}）`,
        `VA ${r.val.toFixed(2)}–${r.vah.toFixed(2)}（含 ${(r.vaPct * 100).toFixed(0)}% 量）`
      );
    }
    lines.push(`样本：每桶平均 ${r.coverAvg.toFixed(1)} 根K线覆盖（≥2 为充分）`);
    let warnTxt = '';
    if (rangeMode === 'auto' && lastAuto) {
      const a = lastAuto;
      lines.push(
        `自适应：ATR60 ${Number.isFinite(a.atr) ? a.atr.toFixed(2) + '%' : '—'}`
        + ` · 容差 ${a.tol.toFixed(1)}% → 取 ${rangeLabel()}`
        + `（现价距 POC ${Number.isFinite(a.dist) ? a.dist.toFixed(2) + '%' : '—'}）`
      );
      if (a.warn === 'far') {
        warnTxt = ' · ⚠POC远离现价（强趋势/高波动），已取最贴近现价的窗口';
        lines.push('⚠ 强趋势或高波动股：任何窗口下 POC 都远离现价，VP 成本中枢参考性下降，建议改用分钟周期');
      }
    }
    el.textContent = `VP · ${rangeLabel()} · ${r.nBars}根/${fmtVol(r.total)}`
      + ` · POC ${r.poc.toFixed(2)} · VA ${r.val.toFixed(2)}–${r.vah.toFixed(2)}（${(r.vaPct * 100).toFixed(0)}%）`
      + `${ztNote}${posTxt}${warnTxt}`;
    el.title = lines.join('\n');
  }

  function rebuild() {
    if (!enabled) {
      result = null;
      slice = null;
      failReason = '';
      updateStatus();
      if (prim) prim.refresh();
      return;
    }
    const w = windowSlice();
    if (!w || w.reason) {
      result = null;
      slice = null;
      failReason = w ? w.reason : '无K线';
      updateStatus();
      if (prim) prim.refresh();
      return;
    }
    const r = compute(w.bars);
    if (!r.ok) {
      result = null;
      slice = null;
      failReason = r.reason;
      updateStatus();
      if (prim) prim.refresh();
      return;
    }
    r.ztN = 0;
    r.dtN = 0;
    if (!isBoard) {
      const t = limitTouches(all, w.w0, w.w1, code, name, period);
      const m = markBins(r.bins, t.zt, t.dt);
      r.ztN = m.nZt;
      r.dtN = m.nDt;
    }

    const price = all.length ? all[all.length - 1].close : null;
    if (Number.isFinite(price) && price > 0) {
      const pp = pricePosition(r.bins, price);
      r.curPrice = price;
      r.pos = pp;
      if (pp.curBin >= 0) r.bins[pp.curBin].cur = 1;
    }
    result = r;
    slice = w;
    failReason = '';
    updateStatus();
    if (prim) prim.refresh();
  }

  /* ---- 渲染 ---- */
  function binColor(b) {
    if (b.poc) return COL.poc;
    if (b.zt) return COL.zt;
    if (b.dt) return COL.dt;
    return b.va ? COL.va : COL.bar;
  }

  function label(ctx, text, x, y, color) {
    ctx.font = '10px -apple-system, "Segoe UI", "Microsoft YaHei", sans-serif';
    const w = ctx.measureText(text).width;
    const ty = y < 14 ? y + 12 : y - 3;
    ctx.fillStyle = 'rgba(19,23,34,0.78)';
    ctx.fillRect(x - 2, ty - 10, w + 4, 12);
    ctx.fillStyle = color;
    ctx.textBaseline = 'alphabetic';
    ctx.fillText(text, x, ty);
  }

  class VpRenderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const o = this._owner;
      const p = o.param;
      const r = result;
      if (!p || !r || !enabled) return;
      const series = p.series;
      const ts = p.chart.timeScale();

      target.useMediaCoordinateSpace(scope => {
        const ctx = scope.context;
        const W = scope.mediaSize.width;
        const H = scope.mediaSize.height;
        const maxW = Math.max(40, W * BAR_MAX_WIDTH_PCT);

        let xR = W - 2;
        try {
          const last = slice ? all[slice.w1] : null;
          const x = last ? ts.timeToCoordinate(last.time) : null;
          if (x != null && x > 0) {
            const bs = ts.options().barSpacing || 7;
            xR = Math.min(W - 2, x + bs * 0.5);
          }
        } catch (_) {}

        const yOf = v => series.priceToCoordinate(v);
        const yTop0 = yOf(r.bins[r.bins.length - 1].hi);
        const yBot0 = yOf(r.bins[0].lo);
        if (yTop0 != null && yBot0 != null) {
          const top = Math.max(-H, Math.min(H, Math.min(yTop0, yBot0)));
          const bot = Math.max(-H, Math.min(H, Math.max(yTop0, yBot0)));
          ctx.fillStyle = 'rgba(41,98,255,0.05)';
          ctx.fillRect(xR - maxW, top, maxW, bot - top);
        }

        for (const b of r.bins) {
          const yT = yOf(b.hi);
          const yB = yOf(b.lo);
          if (yT == null || yB == null) continue;
          const h = Math.max(0.75, yB - yT - 0.5);
          const w = Math.max(1, b.frac * maxW);
          ctx.fillStyle = binColor(b);
          ctx.fillRect(xR - w, yT, w, h);
          if (b.cur && w >= 2) {
            ctx.strokeStyle = '#e2e8f0';
            ctx.lineWidth = 1;
            ctx.strokeRect(xR - w + 0.5, yT + 0.5, w - 1, h - 1);
          }
        }

        const line = (price, color, width, dash) => {
          const y = yOf(price);
          if (y == null) return;
          ctx.save();
          ctx.strokeStyle = color;
          ctx.lineWidth = width;
          if (dash) ctx.setLineDash(dash);
          ctx.beginPath();
          ctx.moveTo(0, y);
          ctx.lineTo(W - 2, y);
          ctx.stroke();
          ctx.restore();
          return y;
        };
        const yPoc = line(r.poc, COL.pocLine, 1.5, null);
        const yVah = line(r.vah, COL.vaLine, 1, [4, 4]);
        const yVal = line(r.val, COL.vaLine, 1, [4, 4]);
        if (yPoc != null) label(ctx, 'POC ' + r.poc.toFixed(2), 6, yPoc, COL.pocLine);
        if (yVah != null) label(ctx, 'VAH ' + r.vah.toFixed(2), 6, yVah, COL.vaLine);
        if (yVal != null) label(ctx, 'VAL ' + r.val.toFixed(2), 6, yVal, COL.vaLine);
      });
    }
  }

  class VpPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new VpRenderer(owner);
    }
    update() {}
    renderer() {
      return enabled && result ? this._renderer : null;
    }
  }

  class VpPrimitive {
    constructor() {
      this.param = null;
      this._view = new VpPaneView(this);
    }
    attached(param) {
      this.param = param;
      this.refresh();
    }
    detached() {
      this.param = null;
    }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    refresh() {
      if (this.param) this.param.requestUpdate();
    }
  }

  // ---------------------------------------------------------------- 浮层
  // 与「形」按钮浮层同款（.ind-params）：点 VP 右侧齿轮打开，装范围选择 + 自定义日期 + 状态行。
  // 按钮本体仍是 VP 总开关；浮层关着时 vpStatus 不存在，updateStatus 空转（getElementById 空安全）。

  function popHtml() {
    return `
      <div class="ind-params-title">成交量分布</div>
      <div class="vp-row"><span>范围</span>
        <select id="vp-range" class="vp-select"></select></div>
      <div class="vp-row vp-custom hidden" id="vp-custom">
        <input type="date" id="vp-start"> <span>~</span> <input type="date" id="vp-end"></div>
      <div id="vpStatus" class="vp-status"></div>`;
  }

  function fillPop() {
    if (!pop) return;
    const sel = pop.el.querySelector('#vp-range');
    const min = isMinute(period);
    for (const [v, txt] of RANGE_OPTIONS) {
      const op = document.createElement('option');
      op.value = v;
      op.textContent = txt;
      op.disabled = !min && DAY_MODES.includes(v);
      sel.appendChild(op);
    }
    sel.value = rangeMode;
    sel.onchange = () => {
      prefMode = sel.value || DEF_RANGE;
      writePref('tv.vp.range', prefMode);
      rangeMode = effectiveMode(period, prefMode);
      if (rangeMode === 'custom' && !customStart && !customEnd) prefillCustom();
      syncCustomUi();
      rebuild();
    };

    const sIn = pop.el.querySelector('#vp-start');
    const eIn = pop.el.querySelector('#vp-end');
    sIn.value = customStart;
    eIn.value = customEnd;
    sIn.onchange = () => {
      customStart = sIn.value || '';
      writePref('tv.vp.start', customStart);
      rebuild();
    };
    eIn.onchange = () => {
      customEnd = eIn.value || '';
      writePref('tv.vp.end', customEnd);
      rebuild();
    };
    syncCustomUi();
    updateStatus();
  }

  function renderPop() {
    if (!pop) return;
    pop.el.innerHTML = popHtml();
    fillPop();
  }

  /* 切标的/周期时浮层若开着，即时重渲染（如分钟档 d1~d5 解禁、rangeMode 变化） */
  function syncPop() {
    if (pop) renderPop();
  }

  function closePop() {
    TV.ui.close(); // onClosed 里置 pop = null
  }

  function openPop(anchor) {
    TV.ui.open({
      id: 'vp-pop', cls: 'vp-pop', anchor, anchorSel: '#vp-toggle',
      render: el => { pop = { el }; renderPop(); },
      onClosed: () => { pop = null; },
    });
  }

  function prefillCustom() {
    if (!all.length) return;
    const n = Math.min(250, all.length);
    customStart = dayKey(all[all.length - n].time);
    customEnd = dayKey(all[all.length - 1].time);
    writePref('tv.vp.start', customStart);
    writePref('tv.vp.end', customEnd);
    const s = document.getElementById('vp-start');
    const e = document.getElementById('vp-end');
    if (s) s.value = customStart;
    if (e) e.value = customEnd;
  }

  function syncCustomUi() {
    const wrap = document.getElementById('vp-custom');
    if (wrap) wrap.classList.toggle('hidden', rangeMode !== 'custom');
    const sel = document.getElementById('vp-range');
    const min = isMinute(period);
    if (sel) {
      for (const op of sel.options) {
        if (DAY_MODES.includes(op.value)) op.disabled = !min;
      }
      if (sel.value !== rangeMode) sel.value = rangeMode;
    }
  }

  function setEnabled(on) {
    enabled = !!on;
    writePref('tv.vp.on', enabled ? '1' : '0');
    const btn = document.getElementById('vp-toggle');
    if (btn) btn.classList.toggle('on', enabled);
    rebuild();
  }

  function setData(bars, per, cd, nm, board = false) {
    all = Array.isArray(bars) ? bars : [];
    period = String(per || 'day');
    code = cd || '';
    name = nm || '';
    isBoard = !!board;
    const want = effectiveMode(period, prefMode);
    if (want !== rangeMode) rangeMode = want;
    if (rangeMode === 'custom' && !customStart && !customEnd) prefillCustom();
    syncPop();
    rebuild();
  }

  function init() {
    prim = new VpPrimitive();
    TV.chart.candle.attachPrimitive(prim);
    const btn = document.getElementById('vp-toggle');
    if (btn) {
      btn.classList.toggle('on', enabled);
      btn.onclick = () => setEnabled(!enabled);
      // 齿轮：点它开成交量分布浮层（不触发开关）
      const gear = document.createElement('i');
      gear.className = 'ind-gear';
      gear.id = 'vp-gear';
      gear.title = '范围选择与状态';
      gear.textContent = '⚙';
      gear.addEventListener('click', e => {
        e.preventDefault();
        e.stopPropagation();
        if (pop) closePop(); else openPop(gear);
      });
      btn.appendChild(gear);
    }
    TV.chart.instance.timeScale().subscribeVisibleTimeRangeChange(() => {
      if (!enabled || rangeMode !== 'visible') return;
      rebuild();
    });
  }

  function debug() {
    return {
      enabled, rangeMode, period, code, name, failReason, auto: lastAuto, pop: !!pop,
      slice: slice ? { w0: slice.w0, w1: slice.w1, n: slice.bars.length } : null,
      result: result ? {
        nBars: result.nBars, total: result.total, poc: result.poc,
        vah: result.vah, val: result.val, vaPct: result.vaPct,
        coverAvg: result.coverAvg, ztN: result.ztN || 0, dtN: result.dtN || 0,
        curPrice: result.curPrice, pos: result.pos,
        bins: result.bins.map(b => ({
          lo: b.lo, hi: b.hi, vol: b.vol, frac: b.frac,
          poc: b.poc, va: b.va, zt: b.zt, dt: b.dt, cur: b.cur,
        })),
      } : null,
    };
  }

  TV.vp = {
    init, setData, setEnabled, refresh: () => rebuild(), debug,
    openPop, closePop,
    isEnabled: () => enabled,
    _internals: {
      limitPct, limitTouches, markBins, compute, coverRange, modeBars,
      pricePosition, effectiveMode, fmtVol, dayKey, atrPct, autoCands, autoWindow,
    },
  };
})();
