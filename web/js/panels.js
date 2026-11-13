(function () {
  const TV = (window.TV = window.TV || {});
  let host = null;

  // panels.js 自己的版本常量：改完本文件必须与 index.html 里 ?v= 一起 bump。
  // 版本戳写死在代码里、而不是从脚本 URL 反解——URL 是「请求了什么」，不是「跑的是什么」：
  // 浏览器命中旧缓存时 URL 仍是新的，徽标就会显示一个根本没执行的版本。
  // 二者不一致（通常是 index.html 改了、这份 JS 还是旧的）时徽标显式标红。
  const TV_JS_BUILD = '20261107a';
  const TV_JS_MISMATCH = (() => {
    const s = (document.currentScript && document.currentScript.src) || '';
    const m = s.match(/[?&]v=([^&]+)/);
    return m && m[1] !== TV_JS_BUILD ? m[1] : '';
  })();

  function root() {
    if (!host || !host.isConnected) host = document.getElementById('side-body');
    return host;
  }

  function section(key, title, toolsHtml) {
    let el = document.getElementById('panel-' + key);
    if (!el) {
      el = document.createElement('section');
      el.id = 'panel-' + key;
      el.className = 'panel';
      el.innerHTML =
        `<div class="panel-head"><span>${title}</span><span class="panel-tools"></span></div>` +
        `<div class="panel-body"></div>`;
      root().appendChild(el);
    }
    if (toolsHtml != null) el.querySelector('.panel-tools').innerHTML = toolsHtml;
    return el.querySelector('.panel-body');
  }

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, c => (
      { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
    ));
  }

  let scanAll = false;

  const HEAT_ORDER_KEY = 'tv.heat.order';
  let heatData = null;
  let heatParent = '';
  let heatParentName = '';
  let heatOrder = 'desc';
  let heatBuilt = false;
  let heatLoading = false;
  try { heatOrder = localStorage.getItem(HEAT_ORDER_KEY) === 'asc' ? 'asc' : 'desc'; } catch (_) {}

  const MARKET_TAB_KEY = 'tv.market.tab';
  let marketTab = 'heat';
  try {
    const v = localStorage.getItem(MARKET_TAB_KEY);
    marketTab = (v === 'indices' || v === 'overview') ? v : 'heat';
  } catch (_) {}
  let indicesData = null;
  let indicesError = '';
  let indicesLoading = false;
  let indicesBuilt = false;
  let indexParent = null;
  let indexMembersLoading = '';
  const indexMembers = new Map();

  let scanSeq = 0; // 竞态防护：快速切股时，旧买卖点表不得覆盖新面板
  let sectorScanView = 'own'; // 板块图下面板视图：板块自身买卖点 ↔ 全市场选股（选股只依赖扫描缓存，与当前图无关）
  async function loadScan(state) {
    const my = ++scanSeq;
    const body = section('scan', state.kind === 'sector'
      ? (sectorScanView === 'scan' ? '缠论买卖点' : '板块买卖点')
      : '缠论买卖点',
      `<button class="mini" id="scan-toggle">${scanAll ? '当前标的' : '全市场'}</button>`);
    // 切全市场/当前标的必须始终可用（当前标的无记录时也要能切）
    document.getElementById('scan-toggle').onclick = async () => {
      scanAll = !scanAll;
      await loadScan(TV.app.state);
    };
    const scanTitle = document.querySelector('#panel-scan .panel-head > span:first-child');
    if (scanTitle) scanTitle.textContent = state.kind === 'sector'
      ? (sectorScanView === 'scan' ? '缠论买卖点' : '板块买卖点')
      : '缠论买卖点';
    if (state.kind === 'sector') {
      // 板块图保留选股能力：按钮在「板块自身买卖点 ↔ 全市场选股」间切换，不再隐藏
      const btn = document.getElementById('scan-toggle');
      btn.style.display = '';
      btn.onclick = async () => {
        sectorScanView = sectorScanView === 'own' ? 'scan' : 'own';
        await loadScan(TV.app.state);
      };
      if (sectorScanView === 'scan') {
        btn.textContent = '板块自身';
        body.innerHTML = '<div class="dim">加载中…</div>';
        let j;
        try {
          j = await TV.api.scan('', 'D', 200);
        } catch (e) {
          if (my !== scanSeq) return;
          body.innerHTML = `<div class="down">${esc(e.message)}</div>`;
          return;
        }
        if (my !== scanSeq) return;
        renderScanTable(body, j, true, state);
        return;
      }
      btn.textContent = '全市场选股';
      const last = TV.chan && TV.chan.last;
      if (!last || !(last.bs || []).length) {
        body.innerHTML = '<div class="dim">板块/指数暂无买卖点；点右上「全市场选股」看全市场缠论买点</div>';
        return;
      }
      const rows = (last.bs || [])
        .filter(p => (p.dt || p.confirm_dt))
        .sort((a, b) => String(b.dt || b.confirm_dt) < String(a.dt || a.confirm_dt) ? -1 : 1);
      body.innerHTML =
        `<div class="dim" style="margin-bottom:4px">板块自身缠论买卖点 · ${rows.length} 条</div>` +
        `<table class="grid grid4"><thead><tr>
           <th>日期</th><th>类型</th><th>方向</th><th class="r">价</th>
         </tr></thead><tbody>` +
        rows.slice(0, 50).map(p => `
        <tr>
          <td>${esc(String(p.dt || p.confirm_dt || '').slice(0, 10))}</td>
          <td>${esc(p.method_label || p.point_type || '')}</td>
          <td class="${p.direction === '买' ? 'up' : 'down'}">${esc(p.direction || '')}</td>
          <td class="r">${p.price == null ? '—' : Number(p.price).toFixed(2)}</td>
        </tr>`).join('') +
        '</tbody></table>';
      return;
    }
    if (!/^\d{6}$/.test(String(state.code || ''))) {
      document.getElementById('scan-toggle').style.display = 'none';
      body.innerHTML = '<div class="dim">该代码不提供个股买卖点</div>';
      return;
    }
    body.innerHTML = '<div class="dim">加载中…</div>';

    let j;
    try {
      j = await TV.api.scan(scanAll ? '' : state.code, 'D', scanAll ? 200 : 50);
    } catch (e) {
      if (my !== scanSeq) return; // 过期请求的错误不覆盖新面板
      body.innerHTML = `<div class="down">${esc(e.message)}</div>`;
      return;
    }
    if (my !== scanSeq) return; // 过期响应丢弃

    renderScanTable(body, j, scanAll, state);
  }

  /** 买卖点表（个股选股与板块图的全市场选股共用）：showCodes=全市场模式显示代码副行并支持点行进个股图 */
  function renderScanTable(body, j, showCodes, state) {
    if (!j.rows.length) {
      body.innerHTML = '<div class="dim">当前无记录</div>';
      return;
    }

    const symbols = (TV.app && TV.app.state && TV.app.state.symbols) || state.symbols || [];
    const names = new Map(symbols.map(s => [s.code, s.name]));
    const nameOf = code => names.get(code)
      || (state.code === code ? state.name : '')
      || '';

    body.innerHTML =
      `<div class="dim" style="margin-bottom:4px">as-of ${esc(j.asof)} · ${j.n} 条</div>` +
      `<table class="grid scan-grid"><thead><tr>
         <th>个股</th><th>日期</th><th>类型</th><th>方向</th><th class="r">价</th>
       </tr></thead><tbody>` +
      j.rows.map(r => {
        const nm = nameOf(r.code);
        const codeLine = showCodes && nm ? `<div class="scan-code">${esc(r.code)}</div>` : '';
        return `
        <tr data-code="${esc(r.code)}" class="click" title="${esc(r.code)} ${esc(nm)}">
          <td class="scan-name"><div class="scan-nm">${esc(nm || r.code)}</div>${codeLine}</td>
          <td>${esc((r.dt || '').slice(5))}</td>
          <td>${esc(r.method_label || r.point_type)}</td>
          <td class="${r.direction === '买' ? 'up' : 'down'}">${esc(r.direction)}</td>
          <td class="r">${Number(r.price).toFixed(2)}</td>
        </tr>`;
      }).join('') +
      '</tbody></table>';

    body.querySelectorAll('tr[data-code]').forEach(tr => {
      tr.addEventListener('click', () => TV.app.open(tr.dataset.code));
    });
    markActive(body);   // 重渲染换掉了整批 tr 节点，当前标的的高亮得补回来（滚轮换股的锚）
  }

  function ensureMarketPanel() {
    const title = marketTab === 'indices' ? '宽基指数'
      : marketTab === 'overview' ? '板块总览' : '行业热度';
    const body = section('heat', title, '<button class="mini" id="heat-refresh">刷新</button>');
    const titleEl = document.querySelector('#panel-heat .panel-head > span:first-child');
    if (titleEl) titleEl.textContent = title;
    const btn = document.getElementById('heat-refresh');
    if (btn) btn.onclick = () => (marketTab === 'indices' ? loadIndices(true)
      : marketTab === 'overview' ? (TV.overview ? TV.overview.load(true) : Promise.resolve())
      : loadHeatData(true));
    return body;
  }

  function marketTabsHtml() {
    return `<div class="market-tabs">` +
      `<button type="button" data-mtab="heat" class="${marketTab === 'heat' ? 'on' : ''}">行业热度</button>` +
      `<button type="button" data-mtab="indices" class="${marketTab === 'indices' ? 'on' : ''}">宽基指数</button>` +
      `<button type="button" data-mtab="overview" class="${marketTab === 'overview' ? 'on' : ''}">板块总览</button>` +
      `</div>`;
  }

  function bindMarketTabs(body) {
    body.querySelectorAll('button[data-mtab]').forEach(btn => {
      btn.onclick = () => switchMarketTab(btn.dataset.mtab);
    });
  }

  // —— 三个横截面页签（行业热度 / 宽基指数 / 板块总览）统一手势 ——
  // 单击 = 只把主图切成该板块/指数的 K 线；双击 = 再把侧栏摊成成分股。
  // 一次双击会先发两记 click，同码已在图上就跳过第二记，免得白跑一整轮 load（bars+指标+缠论）。
  function pickBoard(code, name) {
    const st = TV.app && TV.app.state;
    if (st && st.kind === 'sector' && st.code === code) return;
    if (TV.app.openBoard) TV.app.openBoard(code, name);
  }
  function bindBoardDbl(el, code, name, drill) {
    el.addEventListener('click', () => pickBoard(code, name));
    // drill 内部若也要开图，必须走同一个 pickBoard：此刻 state.code 已等于 code，
    // 直接 openBoard 会把双击第二记省掉的那轮 load 再补发一遍。
    el.addEventListener('dblclick', () => drill());
  }

  // —— 下钻成分股表：三个页签共用一张表（2026-11-02 跟统一手势并成一份）——
  // 载荷字段是历史包袱：热度/总览的行给 heat，指数成分股给 pct，两者都是「当日涨幅 %」，
  // 所以列名只在这里归一一次，排序/取值/着色往下全走 memberPct。
  // 各页签只交代自己的差异：标题、返回落到哪一层、表头之前的附加内容（排序条 / 状态行）。
  function memberPct(r) { return r.heat == null ? r.pct : r.heat; }

  function sortedMembers(rows) {
    const sign = heatOrder === 'asc' ? 1 : -1;
    return rows.slice().sort((a, b) =>
      (memberPct(a) == null) - (memberPct(b) == null)
      || ((memberPct(a) || 0) - (memberPct(b) || 0)) * sign
      || String(a.code).localeCompare(String(b.code)));
  }

  // o = { title, rows, loading, error, note, head, extraNav, backId, backLabel, onBack }
  function renderDrill(body, o) {
    const nav = marketTabsHtml() + '<div class="heat-nav">' + (o.extraNav || '') +
      `<button class="mini" id="${o.backId}">${esc(o.backLabel)}</button>` +
      `<span class="heat-title">${esc(o.title)}</span></div>`;
    const paint = tail => {
      body.innerHTML = nav + (o.head || '') + tail;
      bindMarketTabs(body);
      const back = body.querySelector('#' + o.backId);
      if (back) back.onclick = o.onBack;
    };
    if (o.loading) { paint('<div class="dim">成分股加载中…</div>'); return; }
    if (o.error) { paint(`<div class="down">${esc(o.error)}</div>`); return; }
    const rows = sortedMembers(o.rows || []);
    if (!rows.length) { paint(`<div class="dim">${esc(o.note || '该板块暂无成分股行情')}</div>`); return; }
    paint('<div class="heat-list"><table class="grid heat-stock-grid"><thead><tr>' +
      '<th>个股</th><th class="r">现价</th><th class="r">涨跌</th><th class="r">成交</th>' +
      '</tr></thead><tbody>' +
      rows.map(r => `
        <tr data-stock="${esc(r.code)}" class="click" title="${esc(r.code)} ${esc(r.name || '')}">
          <td class="scan-name"><div class="scan-nm">${esc(r.name || r.code)}</div>
            <div class="scan-code">${esc(r.code)}</div></td>
          <td class="r">${r.price == null ? '—' : Number(r.price).toFixed(2)}</td>
          <td class="r ${heatClass(memberPct(r))}">${heatPct(memberPct(r))}</td>
          <td class="r">${fmtAmount(r.amount)}</td>
        </tr>`).join('') +
      '</tbody></table></div>');
    body.querySelectorAll('tr[data-stock]').forEach(tr => {
      tr.addEventListener('click', () => TV.app.open(tr.dataset.stock));
    });
  }

  function switchMarketTab(tab) {
    marketTab = (tab === 'indices' || tab === 'overview') ? tab : 'heat';
    if (marketTab === 'indices') indexParent = null;
    try { localStorage.setItem(MARKET_TAB_KEY, marketTab); } catch (_) {}
    if (marketTab === 'indices') {
      if (!indicesData && !indicesLoading) loadIndices();
      else renderIndices();
    } else if (marketTab === 'overview') {
      if (TV.overview) TV.overview.load(false);
    } else if (!heatData && !heatLoading) {
      loadHeatData();
    } else {
      renderHeat();
    }
  }

  function heatPct(v) {
    return v == null ? '—' : (v > 0 ? '+' : '') + v.toFixed(2) + '%';
  }

  function heatClass(v) {
    return v == null ? '' : v > 0 ? 'up' : v < 0 ? 'down' : '';
  }

  function fmtAmount(v) {
    if (!Number.isFinite(v)) return '—';
    if (v >= 1e8) return (v / 1e8).toFixed(1) + '亿';
    if (v >= 1e4) return (v / 1e4).toFixed(0) + '万';
    return Math.round(v) + '元';
  }

  function renderHeat() {
    const body = ensureMarketPanel();
    const tabs = marketTabsHtml();
    if (!heatData) {
      body.innerHTML = tabs + (heatLoading ? '<div class="dim">加载中…</div>' : '');
      bindMarketTabs(body);
      return;
    }
    const d = heatData;
    const root = !heatParent;
    const nav = tabs +
      `<div class="heat-nav">` +
      `<button class="mini ${root ? 'on' : ''}" id="heat-root">二级行业</button>` +
      (root ? '' : `<button class="mini" id="heat-back">‹ 返回</button>` +
        `<span class="heat-title">${esc(heatParentName || heatParent)}</span>`) +
      `</div>`;
    const sort =
      `<div class="heat-sort">` +
      `<button class="mini ${heatOrder === 'desc' ? 'on' : ''}" data-heat-order="desc">热度降序 ↓</button>` +
      `<button class="mini ${heatOrder === 'asc' ? 'on' : ''}" data-heat-order="asc">热度升序 ↑</button>` +
      `</div>`;
    const status =
      `<div class="dim heat-status">as-of ${esc(d.updated || '-')} · ${esc(d.source || '')}` +
      `${d.stale ? ' · 旧快照' : ''}${d.loading ? ' · 更新中' : ''}` +
      `${d.error ? ' · ' + esc(d.error) : ''}</div>`;
    let leaders = '';
    if (root && (d.highest || d.lowest)) {
      const item = (label, r) => r
        ? `<div>${label} <b>${esc(r.name)}</b> <span class="${heatClass(r.heat)}">${heatPct(r.heat)}</span></div>`
        : '';
      leaders = `<div class="heat-leaders">${item('最高', d.highest)}${item('最低', d.lowest)}</div>`;
    }

    if (!root) {
      renderDrill(body, {
        title: heatParentName || heatParent,
        rows: (d.stocks && d.stocks[heatParent]) || [],
        extraNav: `<button class="mini" id="heat-root">二级行业</button>`,
        backId: 'heat-back', backLabel: '‹ 返回',
        onBack: () => { heatParent = ''; heatParentName = ''; renderHeat(); },
        head: sort + status,
      });
      bindHeatChrome();
      return;
    }

    const rows = sortedMembers(d.industries || []);
    body.innerHTML = nav + sort + status + leaders +
      `<div class="dim heat-status">${rows.length} 个二级行业 · ${esc(d.metric || '')}`
      + ` · 单击看板块 K 线，双击展开成分股</div>` +
      `<div class="heat-list">` +
      rows.map(r => `
        <button class="heat-item" data-industry="${esc(r.code)}" data-name="${esc(r.name || '')}"
                title="${esc(r.name || '')} · ${heatPct(r.heat)}">
          <div class="heat-top">
            <span class="heat-name">${esc(r.name || r.code)}</span>
            <span class="${heatClass(r.heat)}">${heatPct(r.heat)}</span>
          </div>
          <div class="heat-sub">
            <span>${r.quoted == null ? '—' : r.quoted}/${r.total == null ? '—' : r.total} 有效` +
            ` · 上涨 ${Number.isFinite(Number(r.up_ratio)) ? Number(r.up_ratio).toFixed(0) + '%' : '—'}</span>
            <span>成交 ${fmtAmount(r.amount)}</span>
          </div>
        </button>`).join('') +
      '</div>';
    bindMarketTabs(body);
    bindHeatChrome();
    body.querySelectorAll('button[data-industry]').forEach(btn => {
      bindBoardDbl(btn, btn.dataset.industry, btn.dataset.name, () => {
        heatParent = btn.dataset.industry;
        heatParentName = btn.dataset.name;
        renderHeat();
      });
    });
  }

  // 热度页签特有的控件：根按钮「二级行业」+ 热度排序条（下钻表头也带着这两样）
  function bindHeatChrome() {
    const body = document.getElementById('panel-heat');
    if (!body) return;
    const root = body.querySelector('#heat-root');
    if (root) root.onclick = () => { heatParent = ''; heatParentName = ''; renderHeat(); };
    body.querySelectorAll('button[data-heat-order]').forEach(btn => {
      btn.onclick = () => {
        heatOrder = btn.dataset.heatOrder === 'asc' ? 'asc' : 'desc';
        try { localStorage.setItem(HEAT_ORDER_KEY, heatOrder); } catch (_) {}
        renderHeat();
      };
    });
  }

  async function loadHeatData(force = false) {
    if (heatBuilt && !force) return;
    const body = ensureMarketPanel();
    if (!heatData) {
      body.innerHTML = marketTabsHtml() + '<div class="dim">加载中…</div>';
      bindMarketTabs(body);
    }
    heatLoading = true;
    try {
      const j = await TV.api.heat(!!force);
      heatData = j;
      heatBuilt = true;
      heatLoading = false;
      if (heatParent && !(j.stocks && j.stocks[heatParent])) {
        heatParent = '';
        heatParentName = '';
      }
      renderHeat();
    } catch (e) {
      heatLoading = false;
      if (heatData) {
        heatData = { ...heatData, error: e.message };
        renderHeat();
      } else {
        body.innerHTML = marketTabsHtml() + `<div class="down">${esc(e.message)}</div>`;
        bindMarketTabs(body);
      }
    }
  }

  function loadHeat(force = false) {
    if (marketTab === 'indices') return loadIndices(force);
    if (marketTab === 'overview') return TV.overview ? TV.overview.load(force) : Promise.resolve();
    if (heatBuilt && !force) return Promise.resolve();
    return loadHeatData(force);
  }

  const INDEX_SRC = { local: '本地', tq: 'TQ', tencent: '腾讯', em: '东财' };

  function renderIndices() {
    const body = ensureMarketPanel();
    const tabs = marketTabsHtml();
    if (!indicesData) {
      body.innerHTML = tabs + (indicesLoading
        ? '<div class="dim">加载中…</div>'
        : indicesError ? `<div class="down">${esc(indicesError)}</div>` : '');
      bindMarketTabs(body);
      return;
    }
    const d = indicesData;
    const src = d.sources || {};
    const asof = (d.items || []).map(x => x.date).filter(Boolean).sort().pop()
      || d.updated || '-';
    const status = `<div class="dim heat-status">as-of ${esc(asof)} · ${d.ok_count || 0}/${d.count || 0} 个指数可用` +
      ` · 本地 ${src.local || 0}` +
      (src.tq ? ` · TQ ${src.tq}` : '') +
      (src.tencent ? ` · 腾讯 ${src.tencent}` : '') +
      (src.em ? ` · 东财 ${src.em}` : '') +
      `${indicesError ? ' · ' + esc(indicesError) : ''}</div>`;
    if (indexParent) {
      renderIndexParent(body, status);
      return;
    }
    const items = d.items || [];
    body.innerHTML = tabs + status +
      '<div class="dim heat-status">主要宽基/指数 · 单击看指数 K 线，双击展开成分股</div>' +
      '<div class="heat-list">' +
      items.map(item => {
        const price = item.price == null ? '价格 —' : Number(item.price).toFixed(2);
        const srcLabel = INDEX_SRC[item.source] || item.source || '';
        const when = [item.date, item.time].filter(Boolean).join(' ');
        const detail = [item.code, price, `${srcLabel} ${when}`.trim()]
          .filter(Boolean).join(' · ');
        const errText = item.error
          ? (item.error.includes('所有指数源') ? '本地无日线' : item.error)
          : '';
        const err = errText ? ' · ' + errText : '';
        return `
        <button class="heat-item" data-index="${esc(item.code)}" data-name="${esc(item.name || '')}"
                data-members="${item.members ? 1 : 0}"
                title="${esc(item.name || '')} ${esc(item.code)} · ${esc(price)} · ${heatPct(item.pct)}${esc(err)}">
          <div class="heat-top">
            <span class="heat-name">${esc(item.name || item.code)}${item.members ? ' ›' : ''}</span>
            <span class="${heatClass(item.pct)}">${heatPct(item.pct)}</span>
          </div>
          <div class="heat-sub"><span>${esc(detail)}${esc(err)}</span></div>
        </button>`;
      }).join('') +
      '</div>';
    bindMarketTabs(body);
    body.querySelectorAll('button[data-index]').forEach(btn => {
      bindBoardDbl(btn, btn.dataset.index, btn.dataset.name, () => {
        openIndex(btn.dataset.index, btn.dataset.name, btn.dataset.members === '1');
      });
    });
  }

  // 双击下钻：侧栏摊成该指数的成分股（主图已由单击切到指数 K 线，这里补一发同码即被 dedupe）
  function openIndex(code, name, members) {
    indexParent = { code, name, members };
    if (members && !indexMembers.has(code)) loadIndexMembers(code);
    else if (!members) {
      indexMembers.set(code, { ok: true, rows: [], note: '该指数不提供成分股列表' });
    }
    renderIndices();
    pickBoard(code, name);
  }

  function loadIndexMembers(code) {
    indexMembersLoading = code;
    renderIndices();
    TV.api.indexConstituents(code)
      .then(j => {
        indexMembers.set(code, j && j.ok !== false ? j : { error: j.error || '读取失败', rows: [] });
      })
      .catch(e => {
        indexMembers.set(code, { error: e.message, rows: [] });
      })
      .finally(() => {
        if (indexMembersLoading === code) indexMembersLoading = '';
        if (indexParent && indexParent.code === code) renderIndices();
      });
  }

  function renderIndexParent(body, status) {
    const p = indexParent;
    const base = {
      title: p.name || p.code,
      backId: 'index-back', backLabel: '‹ 返回',
      onBack: () => { indexParent = null; renderIndices(); },
      head: status,
    };
    if (!p.members) {
      renderDrill(body, Object.assign({}, base, { note: '该指数不提供成分股列表' }));
      return;
    }
    const data = indexMembers.get(p.code);
    if (!data || (indexMembersLoading === p.code && !(data.rows || []).length)) {
      if (!data && indexMembersLoading !== p.code) loadIndexMembers(p.code);
      renderDrill(body, Object.assign({}, base, {
        loading: indexMembersLoading === p.code,
        note: '暂无成分股数据',
      }));
      return;
    }
    if (data.error) {
      renderDrill(body, Object.assign({}, base, { error: data.error }));
      return;
    }
    const rows = data.rows || [];
    renderDrill(body, Object.assign({}, base, {
      rows: rows,
      head: status + `<div class="dim heat-status">${esc(p.name)} · 成分股 ${rows.length} 只` +
        `${data.updated ? ' · ' + esc(data.updated) : ''}${data.source ? ' · ' + esc(data.source) : ''}</div>`,
    }));
  }

  async function loadIndices(force = false) {
    if (indicesBuilt && !force) {
      renderIndices();
      return;
    }
    const body = ensureMarketPanel();
    indicesLoading = true;
    indicesError = '';
    if (!indicesData) {
      body.innerHTML = marketTabsHtml() + '<div class="dim">加载中…</div>';
      bindMarketTabs(body);
    }
    try {
      indicesData = await TV.api.marketIndices(!!force);
      indicesBuilt = true;
    } catch (e) {
      indicesError = e.message;
    }
    indicesLoading = false;
    renderIndices();
  }

  const WATCH_KEY = 'tv.watch';

  function watchList() {
    try {
      const a = JSON.parse(localStorage.getItem(WATCH_KEY) || '[]');
      return Array.isArray(a) ? a.filter(x => typeof x === 'string') : [];
    } catch (_) {
      return [];
    }
  }

  function saveWatch(list) {
    try { localStorage.setItem(WATCH_KEY, JSON.stringify(Array.from(new Set(list)).slice(0, 50))); } catch (_) {} // 隐私模式/配额满不抛
  }

  function inWatch(code) {
    return watchList().includes(code);
  }

  function toggleWatch(code) {
    const list = watchList();
    if (list.includes(code)) saveWatch(list.filter(x => x !== code));
    else saveWatch([code, ...list]);
    return inWatch(code);
  }

  function loadWatch() {
    const body = section('watch', `自选 (${watchList().length})`);
    // section() 只在创建时用 title：显式刷新头部计数，否则加减自选后永远停在 boot 值
    const head = document.querySelector('#panel-watch .panel-head > span:first-child');
    if (head) head.textContent = `自选 (${watchList().length})`;
    const list = watchList();
    if (!list.length) {
      body.innerHTML = '<div class="dim">点顶栏 ☆ 添加当前标的</div>';
      return;
    }
    const symbols = (TV.app && TV.app.state.symbols) || [];
    const nameOf = code => {
      const s = symbols.find(x => x.code === code);
      return s ? s.name : '';
    };

    body.innerHTML = list.map(code => `
      <div class="watch-row" data-code="${esc(code)}">
        <span class="click-code"><b>${esc(code)}</b> ${esc(nameOf(code))}</span>
        <button class="mini del" data-del="${esc(code)}">×</button>
      </div>`).join('');

    body.querySelectorAll('.click-code').forEach(el => {
      el.addEventListener('click', () => {
        const code = el.parentElement.dataset.code;
        TV.app.open(code);
      });
    });
    body.querySelectorAll('button[data-del]').forEach(btn => {
      btn.addEventListener('click', ev => {
        ev.stopPropagation();
        saveWatch(watchList().filter(x => x !== btn.dataset.del));
        loadWatch();
        TV.app.syncWatchStar();
      });
    });
  }

  // —— 滚轮换股（2026-11-02 用户定）：K 线图上的滚轮上下 = 在当前展开的个股表里跳上/下一只 ——
  // 翻动序列直接从 DOM 现取（行序即视觉序，排序/筛选/换表自动跟上，不留需要失效的缓存）。
  // 只认两张表：#panel-scan 的买卖点行（tr[data-code]）、#panel-heat 的下钻成分股行
  // （tr[data-stock]）。板块总览表的 data-code 是 881 板块码，故意不收——否则滚轮会去开板块。
  // 接管条件：当前图上的代码就在这张表里，且表里 ≥2 只。不满足（买卖点面板停在「当前标的」
  // 视图时表里只有本尊）就把滚轮交还图表平移，不然连平移都没了。
  const STEP_TABLES = ['#panel-scan tr[data-code]', '#panel-heat tr[data-stock]'];

  function stepRows(sel) {
    const out = [];
    const seen = new Set();
    for (const tr of document.querySelectorAll(sel)) {
      const code = tr.dataset.code || tr.dataset.stock;
      if (!code || seen.has(code)) continue;   // 同股的多个买/卖点算一只
      seen.add(code);
      out.push({ code, el: tr, on: 'row-on' });
    }
    return out;
  }

  // —— 滚轮翻板块（2026-11-02 用户定）：停在行业/指数/板块这一层时，滚轮上下 = 翻上/下一块 ——
  // 三个页签各一张板块表，元素不同但「代码 + 名称」是齐的；顺序即视觉序（排序/筛选自动跟上）。
  // 下钻之后侧栏摊成成分股，这三张表都不在 DOM 里，所以不会跟个股层的翻动打架。
  function boardRows() {
    const body = document.getElementById('panel-heat');
    if (!body) return [];
    const out = [];
    const take = (sel, attr) => body.querySelectorAll(sel).forEach(el => {
      const code = el.dataset[attr];
      if (!code) return;
      out.push({ code, name: el.dataset.name || '', el,
                 on: el.tagName === 'TR' ? 'row-on' : 'on' });
    });
    if (marketTab === 'heat') take('button[data-industry]', 'industry');
    else if (marketTab === 'indices') take('button[data-index]', 'index');
    else take('.ov-table tbody tr[data-code]', 'code');
    return out;
  }

  // 两张表都可能装着当前代码（个股在选股表、又恰好是某板块成分股）：按上面的顺序取先命中的
  function stepList() {
    const st = TV.app && TV.app.state;
    const cur = (st && st.code) || '';
    if (!cur) return null;
    for (const sel of STEP_TABLES) {
      const rows = stepRows(sel);
      const i = rows.findIndex(r => r.code === cur);
      if (i >= 0 && rows.length >= 2) return { rows, i };
    }
    // 板块层：图上开着某板块的 K 线、侧栏还停在这层列表（没下钻）时，滚轮上下 = 翻上/下一块。
    // 当前那块必须在列表里（图上开的是别的板块、或还没点过任何一块时不接管，滚轮照旧平移）。
    if (st.kind === 'sector' && cur) {
      const brows = boardRows();
      const i = brows.findIndex(r => r.code === cur);
      if (i >= 0 && brows.length >= 2) return { rows: brows, i, board: true };
    }
    // 刚下钻到板块（图上还是板块 K 线、侧栏摊开的是它的成分股）也要能翻：这时当前代码
    // 当然不在股票表里，用面板标题认领这张表——三条下钻路径（行业热度 / 宽基指数 / 板块总览）
    // 的 .heat-title 与 openBoard 的 name 同源。i=-1 表示「还没进任何一只」，第一格进第一只，
    // 往上滚第一格进最后一只。标题对不上（比如你看的是别的板块）就交还平移。
    if (st.kind === 'sector' && st.name) {
      const t = document.querySelector('#panel-heat .heat-title');
      const rows = stepRows(STEP_TABLES[1]);
      if (t && rows.length >= 2 && t.textContent.trim() === st.name.trim()) return { rows, i: -1 };
    }
    return null;
  }

  let stepTimer = null, stepAt = -1, stepKey = '';

  // 高亮与列表滚动立即跟手，落图等到手势静默 140ms：一次连滚三格只开最后一张——
  // open() 要走日线+指标+缠论整条链，逐格开既慢又只会闪过中间那一只的图。
  function wheelStep(px) {
    const hit = stepList();
    if (!hit) return false;
    // 同表判定（首尾码 + 根数）：手势中途从上一格的目标接着数，否则三格都指向相邻同一只
    const key = hit.rows.length + ':' + hit.rows[0].code + ':' + hit.rows[hit.rows.length - 1].code;
    const from = stepTimer && stepKey === key ? stepAt : hit.i;
    stepKey = key;
    const dir = px > 0 ? 1 : -1;
    const next = (from < 0 && dir < 0) ? hit.rows.length - 1 : from + dir;   // 板块态首格向上 = 末只
    stepAt = Math.max(0, Math.min(hit.rows.length - 1, next));               // 首尾停住
    const target = hit.rows[stepAt];
    for (const r of hit.rows) r.el.classList.toggle(r.on, r === target);
    target.el.scrollIntoView({ block: 'nearest' });
    clearTimeout(stepTimer);
    stepTimer = setTimeout(() => {
      stepTimer = null;
      // 板块层：开那块板块的 K 线（走 pickBoard，同码不重开）；个股层：开那只股票
      if (hit.board) pickBoard(target.code, target.name);
      else TV.app.open(target.code);
    }, 140);
    return true;
  }

  // 只高亮不滚动：换股每次都重画买卖点表，若这里也 scrollIntoView，用户在表里读到别处会被强行拽回当前标的
  function markActive(body) {
    const cur = (TV.app && TV.app.state.code) || '';
    for (const tr of body.querySelectorAll('tr[data-code], tr[data-stock]')) {
      tr.classList.toggle('row-on', (tr.dataset.code || tr.dataset.stock) === cur);
    }
  }

  // 换股后把侧栏两张表都刷一遍高亮：成分股表不会因换股重渲染（loadHeat 已构建即短路），
  // 只能由 load() 调这里补；买卖点表随后自己重渲染，renderScanTable 末尾自带高亮。
  function refreshActive() {
    for (const id of ['panel-scan', 'panel-heat']) {
      const el = document.getElementById(id);
      if (el) markActive(el);
    }
    // 板块层的列表也标出当前那块（滚轮翻板块后要看得见翻到了哪一块）
    const cur = (TV.app && TV.app.state.code) || '';
    for (const r of boardRows()) r.el.classList.toggle(r.on, r.code === cur);
  }

  // 内部接口：panels/overview.js 不能直接碰本文件的私有状态，
  // 只能通过这个对象读写（页签 / 热度载荷 / 共用渲染函数）。
  // 不是公开 API，第三方脚本不要用。
  const market = {
    get tab() { return marketTab; },
    ensurePanel: ensureMarketPanel,
    tabsHtml: marketTabsHtml,
    bindTabs: bindMarketTabs,
    bindBoardDbl,
    renderDrill,
    pickBoard,
    getHeat() { return heatData; },
    setHeat(j) { heatData = j; heatBuilt = true; },
  };

  TV.panels = { section, esc, loadScan, loadHeat, loadWatch, toggleWatch, inWatch,
                wheelStep, refreshActive,
                watchList,
                market,
                // 版本戳的单一事实源在本文件；overview.js 只取别名。
                build: { code: TV_JS_BUILD, mismatch: TV_JS_MISMATCH } };
})();
