(function () {
  const TV = (window.TV = window.TV || {});
  const PREF_KEY = 'tv.candles.on';
  const SET_KEY = 'tv.candles.set';   // 形态集合：all（TA-Lib 全部 61 个）| rec27（zg 精选 27 个）
  let enabled = true;
  let patternSet = 'all';
  try { enabled = localStorage.getItem(PREF_KEY) !== '0'; } catch (_) {}
  try { if (localStorage.getItem(SET_KEY) === 'rec27') patternSet = 'rec27'; } catch (_) {}

  let rows = [];
  let bubbles = [];
  let pop = null;   // 挂在「形」按钮齿轮下的浮层（与指标参数浮层同款）

  // 已完结且走向负面的形态不再上图
  const HIDE = ['形态失效', '确认超时', '跟踪结束'];

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g,
      c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  function toTime(dateStr, period) {
    if (!TV.time.isIntraday(period)) return dateStr;
    const t = Date.parse(String(dateStr).replace(' ', 'T') + '+08:00');
    return Number.isFinite(t) ? Math.floor(t / 1000) : null;
  }

  function build(bars, signals, period) {
    if (!bars || !bars.length || !signals || !signals.length) return { rows: [], bubbles: [] };
    const n = bars.length;
    const recent = signals.filter(s =>
      typeof s.index === 'number' && s.index >= n - 60 &&
      !HIDE.some(k => String(s.status || '').indexOf(k) >= 0));
    const byTime = new Map();
    for (const s of recent) {
      const time = s.date != null ? toTime(s.date, period) : (bars[s.index] || {}).time;
      if (time == null) continue;
      const dir = s.direction;
      if (!byTime.has(time)) {
        byTime.set(time, {
          time,
          texts: [],
          position: dir === -1 ? 'aboveBar' : 'belowBar',
          color: dir === 1 ? '#ef5350' : dir === -1 ? '#26a69a' : '#9e9e9e',
          shape: dir === 1 ? 'arrowUp' : dir === -1 ? 'arrowDown' : 'circle',
          size: 0.9,
        });
      }
      byTime.get(time).texts.push(s.name || s.label || s.fn);
    }
    const out = [];
    byTime.forEach(r => {
      const extra = r.texts.length > 2 ? '+' + (r.texts.length - 2) : '';
      r.text = r.texts.slice(0, 2).join('·') + extra;
      delete r.texts;
      out.push(r);
    });
    out.sort((a, b) => (a.time < b.time ? -1 : a.time > b.time ? 1 : 0));
    return { rows: out, bubbles: recent };
  }

  function apply() {
    if (TV.chan && TV.chan.setMarkerSource) {
      TV.chan.setMarkerSource('candles', enabled ? rows : []);
    }
    const btn = document.getElementById('candle-toggle');
    if (btn) btn.classList.toggle('on', enabled);
    renderPop();
  }

  function setData(bars, j, period) {
    const r = build(bars, j && j.candle_signals, period);
    rows = r.rows;
    bubbles = r.bubbles;
    TV.candles.last = {
      engine: (j && j.candle_engine) || '',
      patternSet: (j && j.candle_pattern_set) || '',
      all: (j && j.candle_signals) || [],
      bubbles: r.bubbles,
      filtered: (j && j.candle_signals ? j.candle_signals.length : 0) - r.bubbles.length,
    };
    apply();
  }

  function setVisible(on) {
    enabled = !!on;
    try { localStorage.setItem(PREF_KEY, enabled ? '1' : '0'); } catch (_) {}
    apply();
  }

  function toggle() { setVisible(!enabled); }

  // 切形态集合要重取（后端按 pset 重算并单独缓存），缓存键含 pset 不会串数据：
  // 普通 load 即可（各层缓存键已含 pset），不必全链 fresh 重拉把别的标的缓存全挤掉
  function setPattern(v) {
    patternSet = v === 'rec27' ? 'rec27' : 'all';
    try { localStorage.setItem(SET_KEY, patternSet); } catch (_) {}
    if (TV.app && TV.app.load) TV.app.load().catch(e => console.error('形态切换加载失败', e));
  }

  // ---------------------------------------------------------------- 浮层
  // 与指标参数浮层同款（.ind-params）：点「形」右侧齿轮打开，装集合选择 + 近 60 根形态表

  function popHtml() {
    const st = document.getElementById('candle-toggle');
    const on = !!(st && st.classList.contains('on'));
    const last = TV.candles.last || {};
    const bs = (last.bubbles || []).slice().sort((a, b) =>
      String(b.date) < String(a.date) ? -1 : String(b.date) > String(a.date) ? 1 : 0);
    const head = `
      <div class="ind-params-title">蜡烛形态</div>
      <label class="ind-params-row">
        <span>形态集合</span>
        <select id="candle-set" class="mini"
          title="全部 61 = TA-Lib 全量（含纺锤/长实体等描述性形态，仍过关键位+趋势边缘过滤）；rec27 = zg 精选 27 个">
          <option value="all"${patternSet === 'all' ? ' selected' : ''}>全部 61</option>
          <option value="rec27"${patternSet === 'rec27' ? ' selected' : ''}>rec27（zg）</option>
        </select>
      </label>
      <label class="ind-params-row">
        <span>显示标记</span>
        <input type="checkbox" id="candle-on"${on ? ' checked' : ''}>
      </label>`;
    if (!bs.length) {
      return head + '<div class="dim" style="margin:8px 0 2px">近 60 根无关键位形态</div>';
    }
    const list = bs.slice(0, 40).map(s => `
      <tr title="${esc(s.detail || s.baseDetail || '')}">
        <td>${esc(String(s.date || '').slice(5, 10))}</td>
        <td class="scan-name"><div class="scan-nm">${esc(s.name || s.label || s.fn)}</div></td>
        <td class="r ${s.direction === 1 ? 'up' : s.direction === -1 ? 'down' : ''}">${s.direction === 1 ? '↑' : s.direction === -1 ? '↓' : '—'}</td>
        <td title="${esc(s.status || '')}">${esc(s.status || '')}</td>
      </tr>`).join('');
    return head +
      `<div class="dim" style="margin:6px 0 4px">${bs.length} 个关键位活跃形态（近 60 根）</div>
       <div class="candle-list">
         <table class="grid grid4"><thead><tr>
           <th>日期</th><th>形态</th><th class="r">向</th><th>状态</th>
         </tr></thead><tbody>${list}</tbody></table>
       </div>`;
  }

  function bindPop(el) {
    const sel = el.querySelector('#candle-set');
    if (sel) sel.onchange = () => setPattern(sel.value);
    const chk = el.querySelector('#candle-on');
    if (chk) chk.onchange = () => setVisible(chk.checked);
  }

  function renderPop() {
    if (!pop) return;
    pop.el.innerHTML = popHtml();
    bindPop(pop.el);
  }

  function closePop() {
    TV.ui.close(); // onClosed 里置 pop = null
  }

  function openPop(anchor) {
    TV.ui.open({
      id: 'candle-pop', cls: 'candle-pop', anchor, anchorSel: '#candle-toggle',
      render: el => { pop = { el }; renderPop(); },
      onClosed: () => { pop = null; },
    });
  }

  function init() {
    const btn = document.getElementById('candle-toggle');
    if (btn) {
      btn.onclick = toggle;
      // 齿轮：点它开形态浮层（不触发开关）
      const gear = document.createElement('i');
      gear.className = 'ind-gear';
      gear.id = 'candle-gear';
      gear.title = '形态集合与近 60 根列表';
      gear.textContent = '⚙';
      gear.addEventListener('click', e => {
        e.preventDefault();
        e.stopPropagation();
        if (pop) closePop(); else openPop(gear);
      });
      btn.appendChild(gear);
    }
    apply();
  }

  function debug() {
    return { enabled, markers: rows.length, bubbles: bubbles.length, pop: !!pop };
  }

  TV.candles = { init, setData, toggle, setVisible, setPattern, debug, openPop, closePop,
    last: null, patternSet: () => patternSet };
})();
