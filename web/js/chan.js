(function () {
  const TV = (window.TV = window.TV || {});
  const PREF_KEY = 'tv.chan.on';
  let enabled = true;
  try { enabled = localStorage.getItem(PREF_KEY) !== '0'; } catch (_) {}

  let biSeries = null;
  let zsPrim = null;
  let markersApi = null;
  let markerRows = [];
  const markerExtra = new Map();
  let lastBis = [];
  let lastZs = [];
  let lastBs = [];
  let chanSeq = 0; // 竞态防护：切换标的/周期时，旧缠论响应不得标到新图

  function mergedMarkers() {
    const all = enabled ? markerRows.slice() : [];
    markerExtra.forEach(rows => { for (const r of rows) all.push(r); });
    all.sort((a, b) => (a.time < b.time ? -1 : a.time > b.time ? 1 : 0));
    return all;
  }

  function setMarkerSource(name, rows) {
    if (rows && rows.length) markerExtra.set(name, rows);
    else markerExtra.delete(name);
    if (markersApi) markersApi.setMarkers(mergedMarkers());
  }

  function applyButton() {
    const btn = document.getElementById('chan-toggle');
    if (btn) btn.classList.toggle('on', enabled);
  }

  function applyVisibility() {
    if (biSeries) biSeries.applyOptions({ visible: enabled });
    if (zsPrim) zsPrim.setVisible(enabled);
    if (markersApi) markersApi.setMarkers(mergedMarkers());
    applyButton();
  }

  function ensure() {
    if (biSeries) return;
    biSeries = TV.chart.instance.addSeries(LightweightCharts.LineSeries, {
      color: '#5b8def',
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
      crosshairMarkerVisible: false,
      visible: enabled,
    });
    zsPrim = new TV.ZsPrimitive();
    zsPrim.setVisible(enabled);
    TV.chart.candle.attachPrimitive(zsPrim);
    markersApi = LightweightCharts.createSeriesMarkers(TV.chart.candle, []);
    applyButton();
  }

  function push(arr, time, value) {
    const last = arr[arr.length - 1];
    if (last && last.time === time) { last.value = value; return; }
    arr.push({ time, value });
  }

  function visibleBis(bis, bars) {
    if (!bars.length) return [];
    const t0 = bars[0].time;
    const out = [];
    for (const b of bis) {
      // 起点在可视区之前的笔直接丢掉（避免把时间轴撑到更早）
      if (b.sdt < t0) continue;
      const up = b.sd === 'Up';
      push(out, b.sdt, up ? b.low : b.high);
      push(out, b.edt, up ? b.high : b.low);
    }
    return out;
  }

  function visibleZs(zs, bars) {
    if (!bars.length) return [];
    const t0 = bars[0].time;
    return zs
      .filter(z => z.edt >= t0)
      .map(z => ({
        sdt: z.sdt < t0 ? t0 : z.sdt,
        edt: z.edt,
        zd: z.zd,
        zg: z.zg,
        state: z.state || '',
      }));
  }

  function visibleBs(bs, bars) {
    if (!bars.length) return [];
    const t0 = bars[0].time;
    const byTime = new Map();
    for (const p of bs) {
      const t = p.dt || p.confirm_dt;
      if (t == null || t < t0) continue;
      byTime.set(t, {
        time: t,
        position: p.direction === '买' ? 'belowBar' : 'aboveBar',
        color: p.direction === '买' ? '#ef5350' : '#26a69a',
        shape: p.direction === '买' ? 'arrowUp' : 'arrowDown',
        text: p.method_label || p.point_type || '',
        size: 1,
      });
    }
    return Array.from(byTime.values()).sort((a, b) =>
      a.time < b.time ? -1 : a.time > b.time ? 1 : 0);
  }

  async function load(state, fresh = false) {
    ensure();
    const my = ++chanSeq;
    const j = await TV.api.chan(state.code, state.period, fresh, state.kind === 'sector');
    if (my !== chanSeq) return; // 已有更新的 load：丢弃过期响应
    lastBis = visibleBis(j.bis || [], state.bars || []);
    lastZs = visibleZs(j.zs || [], state.bars || []);
    lastBs = visibleBs(j.bs || [], state.bars || []);
    markerRows = lastBs;
    biSeries.setData(lastBis);
    zsPrim.setData(lastZs);
    applyVisibility();
    TV.chan.last = j;
  }

  function clear() {
    lastBis = [];
    lastZs = [];
    lastBs = [];
    markerRows = [];
    if (biSeries) biSeries.setData([]);
    if (zsPrim) zsPrim.setData([]);
    if (markersApi) markersApi.setMarkers([]);
    TV.chan.last = null;
  }

  function setVisible(on) {
    enabled = !!on;
    try { localStorage.setItem(PREF_KEY, enabled ? '1' : '0'); } catch (_) {}
    applyVisibility();
  }

  function toggle() {
    setVisible(!enabled);
  }

  function init() {
    const btn = document.getElementById('chan-toggle');
    if (btn) btn.onclick = toggle;
    applyVisibility();
  }

  function debug() {
    let seriesVisible = null;
    try {
      if (biSeries) seriesVisible = biSeries.options().visible !== false;
    } catch (_) {}
    return {
      visible: enabled,
      bis: lastBis.length,
      zs: lastZs.length,
      bs: lastBs.length,
      markers: markerRows.length,
      markersShown: enabled ? markerRows.length : 0,
      seriesVisible,
      zsVisible: zsPrim ? zsPrim.visible : null,
    };
  }

  TV.chan = {
    init, load, clear, setVisible, toggle, isVisible: () => enabled, debug,
    setMarkerSource,
    last: null,
  };
})();
