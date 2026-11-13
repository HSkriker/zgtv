(function () {
  const TV = (window.TV = window.TV || {});
  let chart = null;
  let candle = null;
  let volume = null;
  let watermark = null;
  const extraPanes = new Map();

  // 主图 / 成交量之外的空窗由调用方按 id 申请（指标副图等）。
  // 份额只在有额外窗时让位，避免指标窗把 K 线压扁。
  function addPane(id) {
    const hit = extraPanes.get(id);
    if (hit) return hit.pane;
    const pane = chart.addPane();
    extraPanes.set(id, { pane });
    rebalance();
    return pane;
  }

  function removeExtraPane(id) {
    const hit = extraPanes.get(id);
    if (!hit) return false;
    extraPanes.delete(id);
    const idx = chart.panes().indexOf(hit.pane);
    if (idx >= 2 && typeof chart.removePane === 'function') chart.removePane(idx);
    rebalance();
    return true;
  }

  // 主图固定 74%，剩余 26% 由成交量与各副图均分：
  // 无副图 = 74/26；1 个副图 = 74/13/13（副图出现不挤压 K 线，量窗与指标窗等高）
  function rebalance() {
    const panes = chart.panes();
    if (!panes.length) return;
    const n = extraPanes.size;
    const main = 0.74;
    const each = (1 - main) / (1 + n);
    panes[0].setStretchFactor(main);
    if (panes[1]) panes[1].setStretchFactor(each);
    // 按 panes 实际顺序分配：下标 ≥2 的 pane 就是副图（Map 插入序在中间删除后会错位）
    for (const pane of panes.slice(2)) pane.setStretchFactor(each);
    placeLegend();
  }

  // 主图例贴「主图 K 线区」左下角：量主图 pane 底边动态设 bottom，
  // 副图（成交量/指标窗）高度随开关变化时图例始终停在主图与副图的分界线上方。
  function placeLegend() {
    const lg = document.getElementById('legend');
    if (!lg || !chart) return;
    requestAnimationFrame(() => {
      try {
        const pr = chart.panes()[0].getHTMLElement().getBoundingClientRect();
        const wr = lg.parentElement.getBoundingClientRect();
        lg.style.bottom = `${Math.max(30, Math.round(wr.bottom - pr.bottom) + 2)}px`;
      } catch (_) {}
    });
  }

  // 打开任一档的默认视图：屏上只放最近 DEFAULT_VIEW_BARS 根，末根贴右边缘（更早的历史仍在
  // 内存，向左拖即可）；历史短到不足这个数（月/季/年、次新股）则按铺满展开，不让 26 根年线
  // 缩成右下角一小撮。所有档走同一机制：定屏上根数 + 右锚最新 ⇒ 切周期只改变覆盖的时间跨度。
  // 旧口径是固定像素密度 2px/根（1600px 视口铺 614 根），蜡烛密到看不出形态，2026-11-03 改成数根数。
  const DEFAULT_VIEW_BARS = 360;

  // 连点两档时，上一帧的排程作废（见下方 viewSeq 守卫）
  let viewSeq = 0;

  function applyDefaultView(barCount) {
    if (!chart) return;
    const my = ++viewSeq;
    // 必须在下一帧落视图：setData 与本函数在同一 tick，而 LWC 把数据提交和视图更新都排进
    // 渲染帧，当帧设的窗口会按**上一份数据**锚位（实测 5 分钟档 setVisibleLogicalRange 整窗
    // 左移 1527 根、60 分钟档右锚点偏出 46 根）。LWC 的帧回调先注册，下一帧轮到这儿时数据已提交。
    requestAnimationFrame(() => {
      if (my !== viewSeq || !chart) return;
      const ts = chart.timeScale();
      const w = ts.width();
      if (!w || !barCount) return;
      // 屏上根数 = 宽 ÷ 间距，所以间距 = 宽 ÷（根数 + 末根右侧留白）就是「刚好铺满」；
      // 历史长于默认根数时按 DEFAULT_VIEW_BARS 截成最近这么多根。不用 ts.fitContent()：它按
      // LWC 合并后的索引空间算，换档瞬间会被其他系列尚未清理的旧点撑大（实测月档铺出 393 根
      // 窗口装 303 根数据）。
      const gap = ts.options().rightOffset;
      ts.applyOptions({ barSpacing: w / (Math.min(barCount, DEFAULT_VIEW_BARS) + gap) });
      // scrollToRealTime 在落帧时才按当时的最后一根算，与数据长度无关；
      // setVisibleLogicalRange 走索引，会被上一份数据的根数带偏
      ts.scrollToRealTime();
    });
  }

  const CROSS_LINES = {
    vertLine: { color: '#758696', width: 1, style: 3, labelBackgroundColor: '#363a45' },
    horzLine: { color: '#758696', width: 1, style: 3, labelBackgroundColor: '#363a45' },
  };

  // 分钟档后端发的是北京时间的真实 UTC 纪元秒，而 LWC 默认按 UTC 渲染日内时钟（差 8 小时）。
  // 日期刻度与十字线统一按 +08:00 格式化；日线及以上 time 是日期串，无时区歧义。
  const BJ_OFFSET = 8 * 3600;
  const pad2 = n => (n < 10 ? '0' + n : '' + n);
  // LWC TickMarkType：0=Year 1=Month 2=DayOfMonth 3=Time 4=TimeWithSeconds
  function fmtTickMark(time, type) {
    if (typeof time === 'number') {
      const d = new Date((time + BJ_OFFSET) * 1000);
      if (type === 0) return `${d.getUTCFullYear()}年`;
      if (type === 1) return `${d.getUTCMonth() + 1}月`;
      if (type === 2) return `${d.getUTCDate()}日`;
      return `${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())}`;
    }
    if (typeof time === 'string') {
      const y = time.slice(0, 4), m = +time.slice(5, 7), day = +time.slice(8, 10);
      if (type === 0) return `${y}年`;
      if (type === 1) return `${m}月`;
      if (type === 2) return `${day}日`;
      return time;
    }
    if (time && typeof time === 'object' && 'year' in time) {
      if (type === 0) return `${time.year}年`;
      if (type === 1) return `${time.month}月`;
      if (type === 2) return `${time.day}日`;
      return `${time.year}-${pad2(time.month)}-${pad2(time.day)}`;
    }
    return String(time);
  }
  function fmtCrossTime(time) {
    if (typeof time === 'number') {
      const d = new Date((time + BJ_OFFSET) * 1000);
      return `${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())}`;
    }
    if (time && typeof time === 'object' && 'year' in time) {
      return `${time.year}-${pad2(time.month)}-${pad2(time.day)}`;
    }
    return String(time);
  }

  // 缩放 = 改可视区间的长度（周期长短），以视窗中点为锚。
  // 键盘 ↑/↓ 与 Shift+滚轮共用这一份，避免两处各写一套缩放口径。
  function zoomBy(factor) {
    const ts = chart && chart.timeScale();
    const r = ts && ts.getVisibleLogicalRange();
    if (!r) return;
    const mid = (r.from + r.to) / 2;
    const half = Math.max(0.5, (r.to - r.from) / 2 * factor);
    ts.setVisibleLogicalRange({ from: mid - half, to: mid + half });
  }

  // 滚轮自己接：LWC 的滚轮语义是 deltaX 平移、deltaY 缩放，**没有**「关掉缩放后 deltaY
  // 落回平移」——关缩放后整条 wheel 直接 return，连 preventDefault 都不做（页面会跟着滚）。
  // 所以把 handleScroll/handleScale 的 mouseWheel 一起关掉（两个都关 LWC 才摘掉自己的
  // wheel 监听），这里按同一符号约定平移：位移 ÷ 间距 = 根数，与内置横滚一致。
  // 侧栏展开了个股表时，滚轮先给「换股」（TV.panels.wheelStep，返回 true 即已接管）。
  // Shift+滚轮 = 缩放周期长短（向上放大=屏上根数变少，与键盘 ↑ 同口径），不再平移；
  // 想平移用 ←/→ 方向键，或在换股不生效时直接滚轮平移。
  function bindWheel(host) {
    host.addEventListener('wheel', ev => {
      // 修饰键让给浏览器（ctrl+滚轮缩放页面、触控板捏合同样的事件）
      if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
      const ts = chart && chart.timeScale();
      const r = ts && ts.getVisibleLogicalRange();
      if (!r) return;
      ev.preventDefault();
      if (ev.shiftKey) {
        // 一格滚轮 = 键盘一次 ↑/↓ 的幅度（0.8 放大 / 1.25 缩小）；触控板的小 delta 连成连续缩放
        const unit = ev.deltaMode === 1 ? 3 : ev.deltaMode === 2 ? 120 : 100;
        const notches = -ev.deltaY / unit;          // 向上滚为正
        if (!notches) return;
        const f = notches > 0 ? Math.pow(0.8, notches) : Math.pow(1.25, -notches);
        zoomBy(Math.min(2, Math.max(0.5, f)));
        return;
      }
      const per = ev.deltaMode === 2 ? 120 : ev.deltaMode === 1 ? 32 : 1;
      const px = Math.abs(ev.deltaX) > Math.abs(ev.deltaY) ? ev.deltaX : ev.deltaY;
      if (px === 0) return;
      if (TV.panels && TV.panels.wheelStep(px)) return;
      const shift = px * per / ts.options().barSpacing;
      ts.setVisibleLogicalRange({ from: r.from + shift, to: r.to + shift });
    }, { passive: false });
  }

  function init(el) {
    chart = LightweightCharts.createChart(el, {
      autoSize: true,
      layout: {
        background: { type: 'solid', color: '#131722' },
        textColor: '#d1d4dc',
        fontSize: 12,
      },
      grid: {
        vertLines: { color: '#1e222d' },
        horzLines: { color: '#1e222d' },
      },
      rightPriceScale: { borderColor: '#2a2e39', scaleMargins: { top: 0.06, bottom: 0.08 } },
      // barSpacing 不给初值：建图时还没有数据，等首份数据落地后由 applyDefaultView 按屏宽算
      timeScale: { borderColor: '#2a2e39', rightOffset: 4, tickMarkFormatter: fmtTickMark },
      // 滚轮改成横向平移，见上方 bindWheel（缩放归键盘 ↑/↓，main.js bindKeys）
      handleScroll: { mouseWheel: false },
      handleScale: { mouseWheel: false },
      crosshair: {
        mode: LightweightCharts.CrosshairMode.Normal,
        ...CROSS_LINES,
      },
      localization: { locale: 'zh-CN', timeFormatter: fmtCrossTime },
    });
    bindWheel(el);

    candle = chart.addSeries(LightweightCharts.CandlestickSeries, {
      upColor: '#ef5350',
      downColor: '#26a69a',
      borderVisible: false,
      wickUpColor: '#ef5350',
      wickDownColor: '#26a69a',
      priceLineVisible: true,
      priceLineColor: 'rgba(117, 134, 150, 0.6)',
      priceLineStyle: LightweightCharts.LineStyle.Dashed,
    });

    const volPane = chart.addPane();
    volume = volPane.addSeries(LightweightCharts.HistogramSeries, {
      priceFormat: { type: 'volume' },
      lastValueVisible: false,
      priceLineVisible: false,
    });
    chart.panes()[0].setStretchFactor(0.74);
    volPane.setStretchFactor(0.26);

    watermark = LightweightCharts.createTextWatermark(chart.panes()[0], {
      horzAlign: 'center',
      vertAlign: 'center',
      lines: [],
    });
    placeLegend();
    window.addEventListener('resize', placeLegend);

    return { chart, candle, volume };
  }

  function setWatermark(code, name, period) {
    if (!watermark) return;
    watermark.applyOptions({
      lines: [
        { text: code || '', color: 'rgba(209, 212, 220, 0.055)', fontSize: 72,
          fontStyle: 'bold', fontFamily: 'Segoe UI, Microsoft YaHei, sans-serif' },
        { text: `${name || ''} ${period || ''}`.trim(),
          color: 'rgba(209, 212, 220, 0.045)', fontSize: 34,
          fontFamily: 'Segoe UI, Microsoft YaHei, sans-serif' },
      ],
    });
  }

  function setMagnet(on) {
    if (!chart) return;
    chart.applyOptions({
      crosshair: {
        mode: on ? LightweightCharts.CrosshairMode.MagnetOHLC
                 : LightweightCharts.CrosshairMode.Normal,
        ...CROSS_LINES,
      },
    });
  }

  TV.chart = {
    init,
    zoomBy,
    setWatermark,
    setMagnet,
    applyDefaultView,
    addPane,
    removeExtraPane,
    get instance() { return chart; },
    get candle() { return candle; },
    get volume() { return volume; },
    get extraPaneCount() { return extraPanes.size; },
  };
})();
