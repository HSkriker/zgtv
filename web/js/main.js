(function () {
  const TV = (window.TV = window.TV || {});
  const state = {
    code: '600519', name: '', period: 'day', kind: 'stock', storeKey: '600519', bars: [],
    colorMode: 'cn', scaleMode: 'normal', magnet: false,
    timeIndex: new Map(), volByTime: new Map(),
    symbols: [], results: [], cursor: -1,
    tags: null,
  };

  const PALETTE = {
    cn: { up: '#ef5350', down: '#26a69a' },
    us: { up: '#26a69a', down: '#ef5350' },
  };
  const colors = () => PALETTE[state.colorMode] || PALETTE.cn;

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g,
      c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  function applyPalette() {
    const c = colors();
    TV.chart.candle.applyOptions({
      upColor: c.up, downColor: c.down,
      wickUpColor: c.up, wickDownColor: c.down,
    });
  }

  function paintVolume() {
    const c = colors();
    TV.chart.volume.setData(state.bars.map(b => ({
      time: b.time, value: b.vol,
      color: b.close >= b.open ? c.up : c.down,
    })));
  }

  function repaint() {
    applyPalette();
    paintVolume();
    if (TV.ind) TV.ind.repaint();
  }

  const fmtVol = TV.time.fmtVol; // 统一口径（time.js 定义，VP 浮层同用）

  function fmtPct(x) {
    if (!Number.isFinite(x)) return '';
    const s = (x * 100).toFixed(2) + '%';
    return x >= 0 ? `<span class="up">+${s}</span>` : `<span class="down">${s}</span>`;
  }

  function renderLegend(bar, idx) {
    const el = document.getElementById('legend-main');
    if (!bar) { el.textContent = ''; return; }
    const prev = idx > 0 ? state.bars[idx - 1] : null;
    const pct = prev ? (bar.close - prev.close) / prev.close : NaN;
    const cls = bar.close >= bar.open ? 'up' : 'down';
    el.innerHTML =
      `<b>${esc(state.code)}</b> ${esc(state.name || '')} · ${esc(TV.time.label(state.period))} &nbsp;` +
      `O <b>${bar.open.toFixed(2)}</b> H <b>${bar.high.toFixed(2)}</b> ` +
      `L <b>${bar.low.toFixed(2)}</b> C <b class="${cls}">${bar.close.toFixed(2)}</b> ` +
      `${fmtPct(pct)} V <b>${fmtVol(bar.vol)}</b>`;
  }

  // 左下角第二行：L2 行业 + 概念题材（个股才有；板块/指数/无归属时为空）
  function renderTags() {
    const el = document.getElementById('legend-tags');
    if (!el) return;
    const t = state.kind === 'stock' ? state.tags : null;
    const ind = t && t.industry;
    const cps = (t && t.concepts) || [];
    const regions = (t && t.regions) || [];
    const styles = (t && t.styles) || [];
    if (!ind && !cps.length && !regions.length && !styles.length) { el.textContent = ''; el.title = ''; return; }
    const parts = [];
    if (ind) parts.push(`行业 <b>${esc(ind.name)}</b> <span class="dim">${esc(ind.code)}</span>`);
    if (cps.length) parts.push(`概念 ${esc(cps.join('、'))}`);
    // 没有概念、只有地区/风格时也要出字：整行为空就没有悬停目标，title 里那两项永远看不到
    if (!cps.length && (regions.length || styles.length)) {
      const cut = (a) => (a.length > 3 ? `${a.slice(0, 3).join('、')}…${a.length}` : a.join('、'));
      if (regions.length) parts.push(`<span class="dim">地区 ${esc(cut(regions))}</span>`);
      if (styles.length) parts.push(`<span class="dim">风格 ${esc(cut(styles))}</span>`);
    }
    // 回退到 2022 年本地 block_gn.dat 时显式标警：数据陈旧是事实，不能静默冒充当前口径
    const legacy = /legacy/.test((t && t.concept_source) || '');
    if (legacy) parts.push('<span class="warn">题材为旧快照</span>');
    el.innerHTML = parts.join(' <span class="dim">·</span> ');

    const src = (t && t.concept_source) || '';
    const at = (t && t.concept_fetched_at) || '';
    const lines = [];
    if (ind) lines.push(`行业（通达信研究行业二级）: ${ind.name} ${ind.code}`);
    if (cps.length) lines.push(`概念题材: ${cps.join('、')}`);
    if (regions.length) lines.push(`地区: ${regions.join('、')}`);
    if (styles.length) lines.push(`风格: ${styles.join('、')}`);
    if (src) lines.push(`题材来源: ${src}${at ? ' · 抓取于 ' + at : ''}`);
    el.title = lines.join('\n');
  }

  async function refreshTags() {
    const code = state.code;
    // 切股瞬间先清旧标签：防上一只股的行业/概念残留显示到新响应回来
    if (state.tags) { state.tags = null; renderTags(); }
    let j = null;
    try {
      j = await TV.api.stockTags(code);
    } catch (e) {
      console.error('个股标签加载失败', e);
    }
    if (state.kind !== 'stock' || state.code !== code) return;
    state.tags = j || null;
    renderTags();
  }

  function normTime(t) {
    if (t == null) return null;
    if (typeof t === 'number' || typeof t === 'string') return t;
    if (t.year) {
      const p = n => String(n).padStart(2, '0');
      return `${t.year}-${p(t.month)}-${p(t.day)}`;
    }
    return null;
  }

  function updateRangeStats(range) {
    const el = document.getElementById('legend-range');
    if (!el) return;
    const n = state.bars.length;
    if (!n) { el.textContent = ''; return; }
    const r = range || TV.chart.instance.timeScale().getVisibleRange();
    const from = r ? normTime(r.from) : null;
    const to = r ? normTime(r.to) : null;
    if (from == null || to == null) {
      el.textContent = ''; return;
    }
    const bars = state.bars;
    let i = 0;
    while (i < n && bars[i].time < from) i++;
    let j = n - 1;
    while (j >= 0 && bars[j].time > to) j--;
    if (j < i) { el.textContent = ''; return; }

    let hi = -Infinity;
    let lo = Infinity;
    for (let k = i; k <= j; k++) {
      if (bars[k].high > hi) hi = bars[k].high;
      if (bars[k].low < lo) lo = bars[k].low;
    }
    const change = bars[i].open ? (bars[j].close - bars[i].open) / bars[i].open : 0;
    const count = j - i + 1;
    el.innerHTML = `区间 H <b>${hi.toFixed(2)}</b> L <b>${lo.toFixed(2)}</b> `
      + `${fmtPct(change)} ${count}根`;
  }

  function bindRange() {
    TV.chart.instance.timeScale()
      .subscribeVisibleTimeRangeChange(r => updateRangeStats(r));
  }

  function bindCrosshair() {
    TV.chart.instance.subscribeCrosshairMove(param => {
      if (TV.draw) TV.draw.setHover(param && param.time != null ? param.time : null);
      if (!param || !param.time) {
        const n = state.bars.length;
        if (n) renderLegend(state.bars[n - 1], n - 1);
        return;
      }
      const c = param.seriesData && param.seriesData.get(TV.chart.candle);
      if (!c) return;
      const idx = state.timeIndex.get(param.time);
      renderLegend(
        { open: c.open, high: c.high, low: c.low, close: c.close,
          vol: state.volByTime.get(param.time) },
        idx == null ? -1 : idx
      );
    });
  }

  // ── 实时行情（方案 C）：15s 心跳；报价 / 大盘状态 / 分钟线共用一道 300s 最短间隔闸 ──
  // 心跳本身不发请求，只检查闸口：换标的、点「刷新」要能在 ≤15s 内补上报价，
  // 而不是等下一个 300s 边界。1 分钟档连报价都不发——后端 minute_series 对 "1"
  // 只读本地 .lc1（离线采集），这里摆一个当日实时价上去只会和分钟线不同源。
  const POLL_MS = 15000;
  const LIVE_MS = 300000;
  let liveTimer = null;
  let liveStatus = null;
  let liveTicks = 0;
  let liveBusy = false;
  // 按绝对时刻设闸（不是 tick 计数）：页面在后台躺了一小时，回到前台第一个心跳就该重新取
  let lastStatusAt = 0;
  let lastQuoteAt = 0;
  let lastBarsAt = 0;
  let barsMeta = null;   // 最近一次 /api/bars 的 meta（1 分钟档的徽标靠它判新鲜度）

  function sourceLabel(s) {
    // MAC 日线把主机写在 source 里（mac@121.36.248.138），按 @ 前缀归类再查表
    const key = String(s || '').split('@')[0];
    return ({
      tq: 'TQ-Local', ths: '同花顺', tencent: '腾讯', em: '东财', sina: '新浪',
      local: '本地DFZQ', cache: '缓存', datahub: 'Datahub', tdx: 'tdx', mac: 'MAC',
    })[key] || (key || '实时');
  }

  function setLiveBadge(html, cls, tip) {
    const el = document.getElementById('live-badge');
    if (!el) return;
    if (!html) { el.classList.add('hidden'); el.innerHTML = ''; return; }
    el.classList.remove('hidden');
    el.className = 'live-badge' + (cls ? ' ' + cls : '');
    el.innerHTML = html;
    // tooltip：把后端 meta.detail 露出来（远端源为何被拒 / 并入了哪个源）。
    // 以前这些原因只进服务端 stderr，页面上什么都没有，只能猜。
    el.title = tip ? `实时行情 / 数据截至 —— ${tip}` : '实时行情 / 数据截至';
  }

  // 1 分钟档的徽标：不发实时报价，改报本地采集的末根时刻（新鲜度由后端 meta.stale 判定）
  function localMinuteBadge() {
    const m = barsMeta || {};
    const stale = !!m.stale;
    // stale = 本地 .lc1 落后最近交易日 → 通达信没做盘后下载，提示一句免得当成程序坏了
    const hint = stale ? '（本地数据未更新）' : '';
    setLiveBadge(`1 分钟 · 本地采集${m.as_of ? ' 截至 ' + esc(m.as_of) : ''}${hint}`,
      stale ? 'stale' : '', m.detail || '');
  }

  // 非交易时段徽标：本地快照通常停在上一交易日，但日/周/月图可能已由远端补到当天。
  // 一律写「本地DFZQ 截至 local_last」会让人以为图也没更新 —— 以最近一次
  // /api/bars 的 as_of/source 为准，本地确实落后时把两个日期都写出来。
  function idleBadge() {
    const m = barsMeta || {};
    const last = (liveStatus && liveStatus.local_last) || '';
    const asOf = m.as_of || '';
    const src = m.source || '';
    const day = asOf.slice(0, 10);
    // 本地已经对平到最新交易日 → 直接报本地（图此时与本地逐根一致，不必提远端）
    const localCurrent = !!last && !!day && last >= day;
    if (src && src !== 'local' && asOf && !localCurrent) {
      return `非交易时段 · ${esc(sourceLabel(src))} ${esc(asOf)}（本地 ${esc(last || '?')}）`;
    }
    // source=local：用**图表自己**的 as_of，不能拿索引的 local_last 顶替 ——
    // 个股自己的 .day 停了（停牌/没下载）时，索引却是新的，拿索引日期会撒谎。
    if (asOf) return `非交易时段 · 本地DFZQ 截至 ${esc(day)}`;
    return `非交易时段 · 本地DFZQ${last ? ' 截至 ' + esc(last) : ''}`;
  }

  // 日/周/月/季/年：末根就是今天时，用实时价改最后一根，避免每次报价都全量重取。
  function updateLastFromQuote(q, price) {
    if (TV.time.isIntraday(state.period)) return;
    const n = state.bars.length;
    if (!n) return;
    const last = state.bars[n - 1];
    const day = String(last.time).slice(0, 10);
    if (day !== q.as_of) return; // 末根不是今天：盘前/周末不硬改
    const open = Number(last.open) || price;
    const high = Math.max(Number(last.high) || price, price);
    const low = Math.min(Number(last.low) || price, price);
    if (last.close === price && last.high === high && last.low === low) return;
    const bar = Object.assign({}, last, { close: price, high, low });
    state.bars[n - 1] = bar;
    state.volByTime.set(bar.time, bar.vol);
    TV.chart.candle.update(bar);
    const c = colors();
    TV.chart.volume.update({
      time: bar.time, value: bar.vol,
      color: bar.close >= bar.open ? c.up : c.down,
    });
    renderLegend(bar, n - 1);
    updateRangeStats();
  }

  function paintLiveQuote(q) {
    const price = Number(q.price);
    const pct = Number(q.pct);
    if (!(price > 0) || !Number.isFinite(pct)) return;
    const cls = pct >= 0 ? 'up' : 'down';
    const t = q.time ? ` · ${esc(q.time)}` : '';
    setLiveBadge(
      `<span class="${cls}">${price.toFixed(2)} ${pct >= 0 ? '+' : ''}${pct.toFixed(2)}%</span>`
      + ` · ${esc(sourceLabel(q.source))}${t}`,
      'live',
      (barsMeta && barsMeta.detail) || '',
    );
    updateLastFromQuote(q, price);
  }

  // 分钟线：远端与本地历史合并由后端完成，这里只需整段重取（TTL 15s）。
  async function refreshLiveBars() {
    if (liveBusy || state.kind === 'sector') return;
    // 竞态防护：轮询期间用户可能已切股/切周期，过期响应一律丢弃（同 load() 的 loadSeq）
    const myCode = state.code;
    const myPeriod = state.period;
    const lmt = TV.time.lmt(myPeriod);
    const pset = TV.candles ? TV.candles.patternSet() : '';
    liveBusy = true;
    try {
      const j = await TV.api.bars(myCode, myPeriod, lmt, true, pset);
      if (myCode !== state.code || myPeriod !== state.period || state.kind !== 'stock') return;
      const bars = j.bars || [];
      if (!bars.length) return;
      barsMeta = j.meta || null;
      state.bars = bars;
      state.timeIndex = new Map(bars.map((b, i) => [b.time, i]));
      state.volByTime = new Map(bars.map(b => [b.time, b.vol]));
      TV.chart.candle.setData(bars);
      repaint();
      if (TV.candles) TV.candles.setData(bars, j, state.period);
      if (TV.pa2Layer) TV.pa2Layer.setData(bars, state.period);
      if (TV.vp) TV.vp.setData(bars, state.period, state.code, state.name, false);
      if (TV.ind) TV.ind.setData(state, true).catch(e => console.error('指标刷新失败', e));
      if (TV.chan) TV.chan.load(state, true).catch(e => console.error('缠论刷新失败', e));
      renderLegend(bars[bars.length - 1], bars.length - 1);
      updateRangeStats();
    } finally {
      liveBusy = false;
    }
  }

  async function liveTick() {
    liveTimer = null;
    if (document.visibilityState === 'visible' && state.kind === 'stock') {
      try {
        if (!liveStatus || Date.now() - lastStatusAt >= LIVE_MS) {
          lastStatusAt = Date.now();
          liveStatus = await TV.api.marketStatus();
        }
        if (!liveStatus.live) {
          const last = liveStatus.local_last || '';
          setLiveBadge(`离线模式（本地DFZQ）${last ? ' 截至 ' + esc(last) : ''}`, '',
            (barsMeta && barsMeta.detail) || '');
        } else if (!liveStatus.session) {
          // 1 分钟档不论盘中盘后都只报本地采集时刻（该档永不接实时源）
          if (state.period === '1') localMinuteBadge();
          else setLiveBadge(idleBadge(), '', (barsMeta && barsMeta.detail) || '');
        } else {
          if (state.period === '1') {
            localMinuteBadge();   // 1 分钟档走离线采集：这一档不发报价
          } else if (Date.now() - lastQuoteAt >= LIVE_MS) {
            // 报价到点才发；失败不记时刻（异常直接跳到 catch），下一个 15s 心跳就重试
            const q = await TV.api.quote(state.code);
            if (q && q.code === state.code) {
              lastQuoteAt = Date.now();
              paintLiveQuote(q);
            }
          }
          // 分钟线不搭报价的顺风车：各档到点才发（1 分钟档也要靠它取回客户端新写入的本地 bar）
          if (TV.time.isIntraday(state.period) && Date.now() - lastBarsAt >= LIVE_MS) {
            lastBarsAt = Date.now();
            await refreshLiveBars();
          }
        }
      } catch (e) {
        console.error('实时行情失败', e);
        if (liveStatus && liveStatus.session) {
          const last = liveStatus.local_last || '';
          setLiveBadge(`实时源不可用 · 本地DFZQ${last ? ' 截至 ' + esc(last) : ''}`, 'stale');
        }
      }
    }
    liveTicks++;
    liveTimer = setTimeout(liveTick, POLL_MS);
  }

  function startLivePoll() {
    if (liveTimer) return;
    liveTimer = setTimeout(liveTick, 1200);
  }

  let loadSeq = 0;
  // 默认可见窗口只在换股/换周期时落一次（同标的同周期的重载——指标开关/形态切换/实时刷新——
  // 保留用户缩放）。窗口口径由 TV.chart.applyDefaultView 统一：所有档同一个屏上根数。
  let lastViewKey = '';
  async function load(fresh = false) {
    const my = ++loadSeq; // 竞态防护：快速切股/切周期时，过期响应不得覆盖新状态
    lastQuoteAt = 0;         // load 一律是用户动作（切股 / 切档 / 点刷新）：报价闸重开，下个心跳（≤15s）就补
    lastBarsAt = Date.now(); // 本次已全量取过 K 线，300s 自动重取从这一刻起算（免得 1s 后再来一遍）
    const isBoard = state.kind === 'sector';
    if (isBoard && TV.time.isIntraday(state.period)) {
      state.period = 'day';
      syncPeriodButtons();
      writeHash();
    }
    const lmt = TV.time.lmt(state.period);
    const pset = TV.candles ? TV.candles.patternSet() : '';
    const j = isBoard
      ? await TV.api.boardBars(state.code, state.period, lmt, fresh, pset)
      : await TV.api.bars(state.code, state.period, lmt, fresh, pset);
    if (my !== loadSeq) return; // 已有更新的 load 接管：丢弃过期响应，state/图表不动
    if (isBoard && j.name) state.name = j.name;
    state.storeKey = isBoard ? 'sector:' + state.code : state.code;
    barsMeta = j.meta || null;
    state.bars = j.bars;
    state.timeIndex = new Map(j.bars.map((b, i) => [b.time, i]));
    state.volByTime = new Map(j.bars.map(b => [b.time, b.vol]));

    TV.chart.instance.applyOptions({
      timeScale: { timeVisible: TV.time.isIntraday(state.period), secondsVisible: false },
    });
    TV.chart.candle.setData(j.bars);
    repaint();
    TV.chart.setWatermark(state.code, state.name, TV.time.label(state.period));
    const viewKey = state.storeKey + ':' + state.period;
    if (viewKey !== lastViewKey) {
      lastViewKey = viewKey;
      TV.chart.applyDefaultView(j.bars.length);
    }
    requestAnimationFrame(() => updateRangeStats());
    if (TV.alerts) TV.alerts.setSymbol(state.storeKey, state.name || '');

    const n = state.bars.length;
    if (n) renderLegend(state.bars[n - 1], n - 1);
    document.title = `${j.name || state.code} ${TV.time.label(state.period)} · tv`;
    if (isBoard) { state.tags = null; renderTags(); } else { refreshTags(); }

    // 年线最多 29 根、次新股的季线也可能不足 60 根，都过不了缠论下限：
    // 不发注定被拒的请求，图层开关直接置灰并写明原因（后端同口径，见 chan.rs）
    const chanBtn = document.getElementById('chan-toggle');
    const tooShort = n < TV.time.MIN_CHAN_BARS;
    if (!TV.time.hasChan(state.period) || tooShort) {
      TV.chan.clear();
      if (chanBtn) {
        chanBtn.disabled = true;
        chanBtn.classList.remove('on');
        chanBtn.title = TV.time.hasChan(state.period)
          ? `${TV.time.label(state.period)}仅 ${n} 根，不足 ${TV.time.MIN_CHAN_BARS} 根，缠论无法计算（上市时间太短）`
          : '年线最多 29 根，缠论需 ≥60 根才可算（本档只有 K 线）';
      }
    } else {
      if (chanBtn) {
        chanBtn.disabled = false;
        chanBtn.title = '显示 / 隐藏缠论图层（笔、中枢、买卖点）';
      }
      try {
        await TV.chan.load(state, fresh);
      } catch (e) {
        console.error('缠论加载失败', e);
        TV.chan.clear();
      }
    }

    if (TV.panels) TV.panels.loadScan(state).catch(e => console.error(e));
    if (TV.panels) TV.panels.loadHeat().catch(e => console.error(e));
    if (TV.panels) TV.panels.refreshActive();   // 侧栏表里当前标的的行高亮（滚轮换股靠它认锚）
    syncWatchStar();
    if (TV.panels) TV.panels.loadWatch();
    try { await TV.draw.load(state); } catch (e) { console.error('画线加载失败', e); }
    if (TV.vp) TV.vp.setData(state.bars, state.period, state.code, state.name, isBoard);
    if (TV.candles) TV.candles.setData(state.bars, j, state.period);
    if (TV.pa2Layer) TV.pa2Layer.setData(state.bars, state.period);
    if (TV.ind) TV.ind.setData(state, fresh).catch(e => console.error('指标加载失败', e));
    if (TV.alerts) TV.alerts.renderPanel();
  }

  function syncPeriodButtons() {
    document.querySelectorAll('#periods button').forEach(btn => {
      const min = TV.time.isIntraday(btn.dataset.period);
      btn.disabled = state.kind === 'sector' && min;
      btn.classList.toggle('on', btn.dataset.period === state.period);
    });
  }

  function bindPeriods() {
    document.querySelectorAll('#periods button').forEach(btn => {
      btn.addEventListener('click', async () => {
        if (state.kind === 'sector' && TV.time.isIntraday(btn.dataset.period)) return;
        if (state.period === btn.dataset.period) return;
        state.period = btn.dataset.period;
        syncPeriodButtons();
        writeHash();
        try { await load(); } catch (e) { showError(e); }
      });
    });
  }

  function bindColorMode() {
    const box = document.getElementById('color-mode');
    state.colorMode = localStorage.getItem('tv.colorMode') === 'us' ? 'us' : 'cn';
    box.checked = state.colorMode === 'us';
    box.addEventListener('change', () => {
      state.colorMode = box.checked ? 'us' : 'cn';
      localStorage.setItem('tv.colorMode', state.colorMode);
      repaint();
    });
  }

  // ---------------- 图表工具 ----------------

  const SCALE_LABEL = { normal: '普通', log: '对数', pct: '百分比' };

  function applyScaleMode() {
    const M = LightweightCharts.PriceScaleMode;
    const mode = state.scaleMode === 'log' ? M.Logarithmic
      : state.scaleMode === 'pct' ? M.Percentage : M.Normal;
    TV.chart.candle.priceScale().applyOptions({ mode });
    const btn = document.getElementById('scale-mode');
    btn.textContent = SCALE_LABEL[state.scaleMode] || SCALE_LABEL.normal;
    btn.classList.toggle('on', state.scaleMode !== 'normal');
  }

  function applyMagnet() {
    TV.chart.setMagnet(state.magnet);
    document.getElementById('magnet').classList.toggle('on', state.magnet);
  }

  function bindTools() {
    const savedScale = localStorage.getItem('tv.scaleMode');
    state.scaleMode = SCALE_LABEL[savedScale] ? savedScale : 'normal';
    state.magnet = localStorage.getItem('tv.magnet') === '1';
    applyScaleMode();
    applyMagnet();

    document.getElementById('scale-mode').addEventListener('click', () => {
      const order = ['normal', 'log', 'pct'];
      state.scaleMode = order[(order.indexOf(state.scaleMode) + 1) % order.length];
      localStorage.setItem('tv.scaleMode', state.scaleMode);
      applyScaleMode();
    });
    document.getElementById('magnet').addEventListener('click', () => {
      state.magnet = !state.magnet;
      localStorage.setItem('tv.magnet', state.magnet ? '1' : '0');
      applyMagnet();
    });
  }

  // ---------------- 搜索 ----------------

  function score(s, q) {
    const code = s.code, name = s.name, py = s.py || '', full = s.pyfull || '';
    if (code === q) return 0;
    if (code.startsWith(q)) return 1;
    if (py === q) return 2;
    if (name === q) return 3;
    if (full.startsWith(q)) return 4;
    if (py.startsWith(q)) return 5;
    if (name.includes(q)) return 6;
    if (full.includes(q)) return 7;
    return 99;
  }

  function search(q) {
    q = q.trim().toLowerCase();
    if (!q) return [];
    return state.symbols
      .map(s => ({ s, k: score(s, q) }))
      .filter(x => x.k < 99)
      .sort((a, b) => a.k - b.k || a.s.code.localeCompare(b.s.code))
      .slice(0, 12)
      .map(x => x.s);
  }

  function renderResults() {
    const drop = document.getElementById('search-drop');
    if (!state.results.length) { drop.classList.add('hidden'); drop.innerHTML = ''; return; }
    drop.innerHTML = state.results.map((s, i) =>
      `<div data-i="${i}" class="${i === state.cursor ? 'on' : ''}">
         <span>${esc(s.code)} ${esc(s.name)}</span><span class="dim">${esc(s.py)}</span>
       </div>`).join('');
    drop.classList.remove('hidden');
    drop.querySelectorAll('div[data-i]').forEach(el => {
      el.addEventListener('mousedown', ev => {
        ev.preventDefault();
        pick(Number(el.dataset.i));
      });
    });
  }

  async function pick(i) {
    const s = state.results[i];
    if (!s) return;
    state.kind = 'stock';
    state.code = s.code;
    state.storeKey = s.code;
    state.name = s.name;
    state.results = [];
    state.cursor = -1;
    document.getElementById('symbol-input').value = `${s.code} ${s.name}`;
    document.getElementById('search-drop').classList.add('hidden');
    syncPeriodButtons();
    writeHash();
    try { await load(); } catch (e) { showError(e); }
  }

  function bindSearch() {
    const input = document.getElementById('symbol-input');
    const drop = document.getElementById('search-drop');

    input.addEventListener('input', () => {
      state.results = search(input.value);
      state.cursor = state.results.length ? 0 : -1;
      renderResults();
    });

    input.addEventListener('keydown', ev => {
      if (ev.key === 'Escape') {
        state.results = []; state.cursor = -1; renderResults(); input.blur(); return;
      }
      if (!state.results.length) return;
      if (ev.key === 'ArrowDown') {
        ev.preventDefault();
        state.cursor = (state.cursor + 1) % state.results.length;
        renderResults();
      } else if (ev.key === 'ArrowUp') {
        ev.preventDefault();
        state.cursor = (state.cursor - 1 + state.results.length) % state.results.length;
        renderResults();
      } else if (ev.key === 'Enter') {
        ev.preventDefault();
        pick(state.cursor);
      }
    });

    input.addEventListener('blur', () => {
      drop.classList.add('hidden');
      // 没选中就恢复当前标的
      const cur = state.symbols.find(s => s.code === state.code);
      input.value = cur ? `${cur.code} ${cur.name}` : state.code;
    });

    drop.addEventListener('mousedown', ev => ev.preventDefault());
  }

  // ---------------- URL 状态 ----------------

  function writeHash() {
    const prefix = state.kind === 'sector' ? 'sector/' : '';
    history.replaceState(null, '', `#${prefix}${state.code}/${state.period}`);
  }

  function readHash() {
    const m = /^#(sector\/)?([A-Za-z0-9]+)\/(day|week|month|quarter|year|1|5|15|30|60)$/
      .exec(location.hash || '');
    if (!m) return;
    if (!m[1] && !/^\d{6}$/.test(m[2])) return;
    state.kind = m[1] ? 'sector' : 'stock';
    state.code = m[2];
    state.storeKey = state.kind === 'sector' ? 'sector:' + state.code : state.code;
    state.period = m[3];
    if (state.kind === 'sector' && TV.time.isIntraday(state.period)) state.period = 'day';
  }

  function showError(e) {
    console.error(e);
    document.getElementById('legend-main').textContent = `加载失败：${e.message}`;
  }

  async function open(code) {
    if (!code) return;
    const wasBoard = state.kind === 'sector';
    state.kind = 'stock';
    if (code === state.code && !wasBoard) return;
    state.code = code;
    state.storeKey = code;
    const s = state.symbols.find(x => x.code === code);
    state.name = s ? s.name : '';
    const input = document.getElementById('symbol-input');
    if (input) input.value = s ? `${s.code} ${s.name}` : code;
    syncPeriodButtons();
    writeHash();
    try { await load(); } catch (e) { showError(e); }
  }

  async function openBoard(code, name) {
    if (!code) return;
    state.kind = 'sector';
    state.code = code;
    state.storeKey = 'sector:' + code;
    state.name = name || '';
    if (TV.time.isIntraday(state.period)) state.period = 'day';
    const input = document.getElementById('symbol-input');
    if (input) input.value = `${code} ${name || ''}`.trim();
    syncPeriodButtons();
    writeHash();
    try { await load(); } catch (e) { showError(e); }
  }

  function syncWatchStar() {
    const btn = document.getElementById('watch-toggle');
    if (!btn) return;
    if (state.kind === 'sector') {
      btn.disabled = true;
      btn.textContent = '☆';
      btn.title = '板块不支持自选';
      return;
    }
    btn.disabled = false;
    btn.title = '加入 / 移出自选';
    btn.textContent = (TV.panels && TV.panels.inWatch(state.code)) ? '★' : '☆';
  }

  function bindWatch() {
    const btn = document.getElementById('watch-toggle');
    btn.addEventListener('click', () => {
      if (state.kind === 'sector') return;
      TV.panels.toggleWatch(state.code);
      syncWatchStar();
      TV.panels.loadWatch();
    });
  }

  const SIDE_KEY = 'tv.sideWidth';

  function applySideWidth(px) {
    document.documentElement.style.setProperty('--side-w', `${px}px`);
  }

  function bindResizer() {
    const saved = Number(localStorage.getItem(SIDE_KEY));
    if (saved >= 220 && saved <= 640) applySideWidth(saved);

    const rz = document.getElementById('side-resizer');
    let dragging = false;

    rz.addEventListener('mousedown', ev => {
      dragging = true;
      ev.preventDefault();
      document.body.style.cursor = 'col-resize';
    });
    window.addEventListener('mousemove', ev => {
      if (!dragging) return;
      const w = Math.min(640, Math.max(220, window.innerWidth - ev.clientX));
      applySideWidth(w);
    });
    window.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      document.body.style.cursor = '';
      const w = parseInt(getComputedStyle(document.documentElement)
        .getPropertyValue('--side-w'), 10) || 300;
      localStorage.setItem(SIDE_KEY, String(w));
    });
    rz.addEventListener('dblclick', () => {
      applySideWidth(300);
      localStorage.setItem(SIDE_KEY, '300');
    });
  }

  // ---------------- 键盘 / 双击 / 区间 ----------------

  function zoomBy(factor) {
    // 缩放口径只有一份：chart.js 的 zoomBy（键盘 ↑/↓ 与 Shift+滚轮共用）
    if (TV.chart && TV.chart.zoomBy) TV.chart.zoomBy(factor);
  }

  function resetPriceScale() {
    TV.chart.candle.priceScale().applyOptions({ autoScale: true });
    if (TV.chart.volume) TV.chart.volume.priceScale().applyOptions({ autoScale: true });
    // 指标副图/主图叠加也要一起复位，否则新增窗后双击价格轴只有前两个窗生效
    if (TV.ind) TV.ind.resetScales();
  }

  function isTyping(el) {
    return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
  }

  function bindKeys() {
    document.addEventListener('keydown', ev => {
      if (ev.defaultPrevented) return;

      if (ev.key === 'Escape') {
        if (TV.menu && TV.menu.close()) return;
        if (isTyping(ev.target)) return;
        if (TV.draw && TV.draw.escape()) ev.preventDefault();
        return;
      }

      if (isTyping(ev.target)) return;
      if (ev.ctrlKey || ev.metaKey || ev.altKey) return;

      const ts = TV.chart.instance.timeScale();
      if (ev.key === 'ArrowLeft' || ev.key === 'ArrowRight') {
        const r = ts.getVisibleLogicalRange();
        if (!r) return;
        ev.preventDefault();
        const step = Math.max(1, Math.round((r.to - r.from) * 0.1));
        const d = ev.key === 'ArrowLeft' ? -step : step;
        ts.setVisibleLogicalRange({ from: r.from + d, to: r.to + d });
      } else if (ev.key === 'ArrowUp' || ev.key === '+' || ev.key === '=') {
        ev.preventDefault();
        zoomBy(0.8);          // ↑ 放大（屏上根数变少）
      } else if (ev.key === 'ArrowDown' || ev.key === '-' || ev.key === '_') {
        ev.preventDefault();
        zoomBy(1.25);         // ↓ 缩小；preventDefault 同时挡住页面滚动
      } else if (ev.key === 'Home') {
        ev.preventDefault();
        ts.fitContent();
      }
    });
  }

  function bindDblClick() {
    const host = document.getElementById('chart');
    host.addEventListener('dblclick', ev => {
      if (TV.draw && TV.draw.state.mode !== 'none') return;
      const panes = TV.chart.instance.panes();
      if (!panes.length) return;
      const dataW = TV.chart.instance.timeScale().width();

      // 指针落在哪个窗就按哪个窗判价格轴；不能假定只有主图（指标副图会改变窗数与高度）
      let hit = null;
      for (const p of panes) {
        const el = p.getHTMLElement();
        if (!el) continue;
        const r = el.getBoundingClientRect();
        if (ev.clientY >= r.top && ev.clientY <= r.bottom) { hit = { el, r }; break; }
      }
      if (hit) {
        if (ev.clientX - hit.r.left > dataW) {
          ev.preventDefault();
          resetPriceScale();
        }
        return;
      }

      const last = panes[panes.length - 1].getHTMLElement().getBoundingClientRect();
      if (ev.clientY >= last.bottom) {
        ev.preventDefault();
        TV.chart.instance.timeScale().fitContent();
      }
    }, true);
  }

  async function boot() {
    TV.chart.init(document.getElementById('chart'));
    TV.draw.init();
    TV.alerts.init();
    TV.vp.init();
    TV.chan.init();
    if (TV.candles) TV.candles.init();
    if (TV.pa2Layer) TV.pa2Layer.init();
    if (TV.ind) TV.ind.init();
    bindTools();
    bindColorMode();
    bindPeriods();
    bindCrosshair();
    bindRange();
    bindKeys();
    bindDblClick();
    bindSearch();
    bindWatch();
    bindResizer();
    if (TV.menu) TV.menu.init();
    readHash();
    syncPeriodButtons();

    document.getElementById('refresh').addEventListener('click', () => load(true).catch(showError));

    try {
      const j = await TV.api.symbols();
      state.symbols = j.rows;
      const cur = state.symbols.find(s => s.code === state.code);
      if (cur) state.name = cur.name;
      document.getElementById('symbol-input').value = cur ? `${cur.code} ${cur.name}` : state.code;
    } catch (e) {
      console.error('标的表加载失败', e);
    }

    try { await load(); } catch (e) { showError(e); }
    // 首屏画完再开轮询：15s 递归 setTimeout（不用 interval），页面隐藏时跳过取数
    startLivePoll();
  }

  document.addEventListener('DOMContentLoaded', boot);
  TV.app = { state, load, open, openBoard, syncWatchStar,
    liveState: () => ({ ticks: liveTicks, status: liveStatus }) };
})();
