(function () {
  const TV = (window.TV = window.TV || {});
  const P = TV.panels;
  if (!P || !P.market) { console.error('overview.js 必须在 panels.js 之后加载'); return; }

  const { section, esc } = P;
  const M = P.market;

  // 版本戳的单一事实源在 panels.js：这里只取别名，
  // 让搬迁过来的状态行代码保持原样（final_check 的版本门仍按原名匹配）。
  const TV_JS_BUILD = P.build.code;
  const TV_JS_MISMATCH = P.build.mismatch;

  let overviewData = null;
  let overviewError = '';
  let overviewLoading = false;
  let overviewBuilt = false;
  let overviewSort = 'rank6';
  let overviewSortAsc = false;
  // 板块总览下钻：点击行业后面板原地变成成份股列表（'' = 总览表；数据复用热度载荷）
  let ovParent = '';
  let ovParentName = '';
  let ovDrillLoading = false;
  const OV_MODE_KEY = 'tv.ov.mode';
  let ovMode = 'rot';   // rot=合成拥挤×Rk6 / comp=×合成分 / ir=×信息比率 / rs=双窗RS
  try {
    const v = localStorage.getItem(OV_MODE_KEY);
    if (v === 'rot' || v === 'comp' || v === 'ir' || v === 'rs') ovMode = v;
  } catch (_) {}
  let ovTraj = false;   // 散点 20 日轨迹
  try { ovTraj = localStorage.getItem('tv.ov.traj') === '1'; } catch (_) {}
  // 六类图例的点击筛选（'' = 不筛）。刻意不落 localStorage：筛着看着像「列表坏了」，
  // 下次进总览应当看到全量，所以只活在本页这一次会话里。
  let ovCat = '';
  let ovRs = false;     // RS≥90 主线筛选（同 ovCat，会话级）

  // ── 板块总览（本地重算：轮动/拥挤/广度；估值·资金离线不可得，本轮未接） ──
  const OV_LABEL_COLOR = {
    '共振占优_尚不拥挤': '#2f9e44',
    '共振占优_高拥挤': '#e8590c',
    '中性观察': '#868e96',
    '底部酝酿_低拥挤': '#1c7ed6',
    '底部观察': '#4dabf7',
    '底部高拥挤_回避': '#9c36b5',
    '数据不足': '#5c616b',
  };
  const OV_SHORT = {
    '共振占优_尚不拥挤': '占优',
    '共振占优_高拥挤': '过热',
    '中性观察': '中性',
    '底部酝酿_低拥挤': '筑底',
    '底部观察': '底部',
    '底部高拥挤_回避': '回避',
    '数据不足': '—',
  };
  function ovColor(label) { return OV_LABEL_COLOR[label] || '#868e96'; }
  // 分类/配色/筛选一律走 label_comp（Rk6×五分量合成拥挤）；旧缓存无该字段时回退 label
  function ovLabel(r) { return r.label_comp || r.label; }
  // 六类口径（阈值逐字对齐 src/overview.rs 的 label_of：Rk6 × 合成拥挤分位，0-100）
  const OV_CAT_TIP = {
    '共振占优_尚不拥挤': '占优：Rk6 ≥ 70 且 合成拥挤 < 70 —— 强且不拥挤，轮动主线',
    '共振占优_高拥挤': '过热：Rk6 ≥ 70 且 合成拥挤 ≥ 70 —— 强但已拥挤，警惕回撤',
    '中性观察': '中性：30 < Rk6 < 70 —— 中间地带，不给方向',
    '底部酝酿_低拥挤': '筑底：Rk6 ≤ 30 且 合成拥挤 ≤ 30 —— 弱且不拥挤，关注启动',
    '底部观察': '底部：Rk6 ≤ 30 且 30 < 合成拥挤 < 70 —— 弱，拥挤度中性',
    '底部高拥挤_回避': '回避：Rk6 ≤ 30 且 合成拥挤 ≥ 70 —— 弱且拥挤，不参与（回放提示：短期或有放量反弹、中期仍弱）',
  };

  function signed(v, dp) {
    if (v == null || !Number.isFinite(v)) return '—';
    return (v > 0 ? '+' : '') + v.toFixed(dp == null ? 2 : dp) + '%';
  }
  function numCls(v) {
    return v == null || !Number.isFinite(v) ? '' : v > 0 ? 'up' : v < 0 ? 'down' : '';
  }
  function plain(v, dp) {
    if (v == null || !Number.isFinite(v)) return '—';
    return v.toFixed(dp == null ? 1 : dp);
  }
  // 0-100 背景条：低蓝 → 高金
  function lerpColor(t) {
    t = Math.max(0, Math.min(1, t));
    const r = Math.round(0x23 + (0xd9 - 0x23) * t);
    const g = Math.round(0x78 + (0xa4 - 0x78) * t);
    const b = Math.round(0xc9 + (0x00 - 0xc9) * t);
    return `rgb(${r},${g},${b})`;
  }
  function bcell(v) {
    if (v == null || !Number.isFinite(v)) return '<td class="r ov-num">—</td>';
    const w = Math.max(0, Math.min(100, v));
    return `<td class="ov-bcell"><span class="ov-fill" style="width:${w.toFixed(0)}%;background:${lerpColor(w / 100)}"></span><b>${w.toFixed(0)}</b></td>`;
  }

  function ovSortVal(r, key) {
    switch (key) {
      case 'rank6': return r.mom_rank6;
      case 'rankc': return r.mom_rank_c;
      case 'd20': return r.rank6_d20;   // 表格 Δ20 固定 Rk6 口径（外圈才随模式走）
      case 'day': return r.day_ret;
      case 'm6': return r.mom && r.mom['6M'];
      case 'crowd': return r.crowd_comp != null ? r.crowd_comp : r.crowd_pct;
      case 'rs': return r.rs_comp;
      case 'amt': return r.amt_x;
      case 'ma20': return r.breadth && r.breadth.above_ma20_pct;
      case 'nh': return r.breadth && r.breadth.nh60_pct;
      case 'rps': return r.breadth && r.breadth.rps63_pct;
      case 'hi': return r.off_hi250;
      default: return null;
    }
  }
  function sortOverview(rows) {
    const dir = overviewSortAsc ? 1 : -1;
    return rows.slice().sort((a, b) => {
      const va = ovSortVal(a, overviewSort), vb = ovSortVal(b, overviewSort);
      const cmp = () => String(a.name).localeCompare(String(b.name), 'zh');
      if (va == null && vb == null) return cmp();
      if (va == null) return 1;   // 空值恒排末尾
      if (vb == null) return -1;
      return (va - vb) * dir || cmp();
    });
  }

  const OV_MODES = { rot: '轮动', comp: '合成', ir: 'IR', rs: '双窗' };
  // 模式按钮悬浮简介（口径与 overview.rs note_long 对齐）
  const OV_MODE_TIP = {
    rot: '轮动：横轴=合成拥挤分位（成交额占比/振幅/β/量价相关/乖离率五分量等权分位合成，全量价类、无估值维度），纵轴=Rk6（6 个月相对强度的截面排名）。'
      + '左上=强且不拥挤（主线）；右上=强但拥挤（过热，警惕回撤）；左下=弱且不拥挤（关注筑底）；右下=弱且拥挤（回避）'
      + '；外圈=Rk6 排名的 20 日位移（百分位点，红走强/绿走弱）',
    comp: '合成：横轴=合成拥挤分位，纵轴=RkC 合成分（IBD 权重 0.4×3M+0.2×6M+0.2×9M+0.2×12M 的截面排名），'
      + '比 Rk6 平滑、少单窗噪声，作对照'
      + '；外圈=RkC 排名的 20 日位移（百分位点，红走强/绿走弱）',
    ir: 'IR：横轴=合成拥挤分位，纵轴=信息比率排名（252 日对 000300 的日均超额 ÷ 超额波动的截面百分位，国盛趋势度口径）。'
      + '比原始动量排名稳，过滤「快但颠」的行业；回放 IC 优于 Rk6/RkC'
      + '；外圈=IR 排名的 20 日位移（百分位点，红走强/绿走弱）',
    rs: '双窗：横轴=RS63（63 日相对 000300 的超额，百分点），纵轴=RS252（252 日）。'
      + '右上=领涨（长短窗都强）；右下=反转启动（短强长弱）；左上=回调蓄势（长强短弱）；左下=回避'
      + '；外圈=RS252 超额的 20 日位移（百分点，红走强/绿走弱）',
  };
  // 模式按钮组：侧栏与大窗共用一份生成器；idSuffix 区分大窗内副本，避免重复 id
  function ovModeBtns(idSuffix) {
    return Object.entries(OV_MODES).map(([k, lab]) =>
      `<button class="mini${ovMode === k ? ' on' : ''}" data-ovmode="${k}" title="${esc(OV_MODE_TIP[k])}">${lab}</button>`).join('')
      + `<span class="ov-mgap"></span>`
      + `<button class="mini${ovTraj ? ' on' : ''}" id="ov-traj${idSuffix}" title="散点叠加最近 20 个交易日轨迹">轨迹</button>`;
  }
  // 散点横轴的拥挤分位：新口径 crowd_comp（五分量合成），旧缓存只有单分量 crowd_pct 时回退
  // （静态文件与后端二进制解耦，浏览器可能拿到新 JS + 旧进程数据；同 ovLabel/排序键/bcell 的处理）
  function ovCrowdX(r) { return r.crowd_comp != null ? r.crowd_comp : r.crowd_pct; }
  function ovCrowdLabel(r) {
    return r.crowd_comp != null ? '合成拥挤' : r.crowd_pct != null ? '拥挤(单分量)' : '拥挤';
  }
  // 当日散点坐标（按模式）；rot/comp/ir 为 0-100 分位（横轴=合成拥挤），rs 为百分点（动态量程）
  function ovCurPt(r) {
    if (ovMode === 'rs') return (r.rs63 != null && r.rs252 != null) ? { x: r.rs63, y: r.rs252 } : null;
    const cx = ovCrowdX(r);
    if (cx == null) return null;
    if (ovMode === 'comp') return r.mom_rank_c != null ? { x: cx, y: r.mom_rank_c } : null;
    if (ovMode === 'ir') return r.mom_rank_ir != null ? { x: cx, y: r.mom_rank_ir } : null;
    return r.mom_rank6 != null ? { x: cx, y: r.mom_rank6 } : null;
  }
  // 轨迹某日坐标（traj_quad 条目 e；cc=当日合成拥挤，逐日 point-in-time 回算；c=旧单分量口径）
  function ovTrajPt(e) {
    if (!e) return null;
    if (ovMode === 'rs') return (e.a != null && e.b != null) ? { x: e.a, y: e.b } : null;
    const cx = e.cc != null ? e.cc : e.c;
    if (cx == null) return null;
    if (ovMode === 'comp') return e.q != null ? { x: cx, y: e.q } : null;
    if (ovMode === 'ir') return e.ir != null ? { x: cx, y: e.ir } : null;
    return e.k != null ? { x: cx, y: e.k } : null;
  }
  // 散点加速度外圈：口径统一为「纵轴指标自身的 20 日位移」，四模式各跟各的轴走
  //   rot : Rk6 排名的 20 日位移（百分位点）
  //   comp: RkC 排名的 20 日位移（百分位点）
  //   ir  : IR 排名的 20 日位移（百分位点）
  //   rs  : RS252 超额的 20 日位移（百分点）
  // 阈值按口径分两套（百分位点 5/15、百分点 3/8），但每个口径画出来的密度并不相同，
  // 下面是同一批 127 个有值行业在各自主轴的实测占比（2026-11-03 as_of，
  // 取 `tv --replay-overview` 的 rank6_d20/rankc_d20/mom_rank_ir_d20/rs252_d20 四列）：
  //   rot 83% 画环 / 61% 加粗 · comp 81%/47% · ir 74%/35% · rs 80%/53%
  // 即 IR 明显比 Rk6 稀疏（IR 本身更平滑），这是口径差异不是阈值失配；
  // 别再按「一个数字代表三套百分位点口径」写死，也别只换一处阈值。
  const OV_ACCEL = {
    rot: { key: 'rank6_d20', unit: '百分位点', draw: 5, strong: 15, name: 'Rk6 排名' },
    comp: { key: 'rankc_d20', unit: '百分位点', draw: 5, strong: 15, name: 'RkC 排名' },
    ir: { key: 'mom_rank_ir_d20', unit: '百分位点', draw: 5, strong: 15, name: 'IR 排名' },
    rs: { key: 'rs252_d20', unit: '百分点', draw: 3, strong: 8, name: 'RS252 超额' },
  };
  function ovAccelSpec() { return OV_ACCEL[ovMode] || OV_ACCEL.rot; }
  // 原始位移（不看阈值）：tooltip 用，缺失即 null（不留 0 假装有值）
  function ovAccelRaw(r) {
    const a = ovAccelSpec();
    const v = r[a.key];
    if (v == null || !Number.isFinite(v)) return null;
    return { v, unit: a.unit, name: a.name };
  }
  // 过阈值后用于画环：低于噪声门不画
  function ovAccel(r) {
    const raw = ovAccelRaw(r);
    if (!raw) return null;
    const a = ovAccelSpec();
    if (Math.abs(raw.v) < a.draw) return null;
    return { ...raw, strong: Math.abs(raw.v) >= a.strong };
  }
  // 外圈可见度：只有环是 1px 细线，所以不能跟点一起压到 15%（深底上等于隐形）。
  // 强弱由基准值表达（强 0.95 / 弱 0.5），筛选态再整体乘 dim 系数（0.85）——
  // 单一乘数保证单调：未筛强 0.95 > 筛选强 0.81 > 未筛弱 0.5 > 筛选弱 0.43。
  // 旧写法把筛选态的 stroke-opacity 顶成 1、只留 opacity 0.62，
  // 结果 0.62 > 未筛选弱环的 0.5，非目标的环反而比目标的弱环更显眼。
  function ovRingOpacity(dim, strong) {
    return (strong ? 0.95 : 0.5) * (dim ? 0.85 : 1);
  }
  function ovDotTip(r) {
    const m = r.mom || {};
    const yName = ovMode === 'comp' ? 'RkC' : ovMode === 'ir' ? 'IR' : ovMode === 'rs' ? 'RS252' : 'Rk6';
    const yVal = ovMode === 'comp' ? r.mom_rank_c : ovMode === 'ir' ? r.mom_rank_ir
      : ovMode === 'rs' ? r.rs252 : r.mom_rank6;
    const pair = ovMode === 'rs'
      ? `RS63 ${signed(r.rs63)}  RS252 ${signed(r.rs252)}`
      : `${ovCrowdLabel(r)} ${plain(ovCrowdX(r), 0)}  ${yName} ${plain(yVal, 0)}`;
    const parts = `份额${plain(r.crowd_pct, 0)} 振幅${plain(r.crowd_vol_pct, 0)} β${plain(r.crowd_beta_pct, 0)} 量价${plain(r.crowd_corr_pct, 0)} 乖离${plain(r.crowd_bias_pct, 0)}`;
    // 单位不要复用 signed()：它自带 '%'，再拼「百分位点/百分点」会变成「-1.57%百分位点」，
    // 而排名位移本身也不是百分比变化。先按显示精度归零再判方向，避免 -0.004 显示「-0.00 走弱」。
    const acc = ovAccelRaw(r);
    let accTxt = '';
    if (acc) {
      const av = Math.abs(acc.v) < 0.005 ? 0 : acc.v;
      const drawn = Math.abs(av) >= ovAccelSpec().draw;
      accTxt = `\n外圈 ${acc.name} 20日位移 ${av > 0 ? '+' : ''}${av.toFixed(2)} ${acc.unit}`
        + `（${av > 0 ? '走强' : av < 0 ? '走弱' : '持平'}）`
        + (drawn ? '' : ' · 低于画环阈值');
    }
    const bd = r.breadth || {};
    return `${r.name} ${r.code}\n${pair}\n拥挤分量 ${parts}\n今日 ${signed(r.day_ret)}  6M ${signed(m['6M'])}`
      + `\nRS合成 ${plain(r.rs_comp, 0)}${r.rs90 ? ' ★主线(RS≥90)' : ''}  ${ovLabel(r)}`
      + `\n成分RPS≥90 季 ${plain(bd.rps63_pct, 0)}% / 月 ${plain(bd.rps20_pct, 0)}%`
      + accTxt
      + `\n单击 = 板块 K 线 · 双击 = 成分股`;
  }

  // 六类图例（侧栏与放大窗共用一份）：悬停给口径，点击把下面列表筛成该类，再点取消。
  // 分类恒按 Rk6 × 合成拥挤判（与散点当前纵轴是哪根无关——合成/IR/双窗模式只是换纵轴看同一个点）。
  // 计数为 0 且未激活的类才 disabled；已激活的即使计数归零也保持可点，否则筛选无法取消。
  function ovLegendHtml(rows) {
    const cnt = new Map();
    for (const r of rows) cnt.set(ovLabel(r), (cnt.get(ovLabel(r)) || 0) + 1);
    const nRs = rows.filter(r => r.rs90).length;
    return `<div class="ov-legend" title="悬停看每类口径；点击把下面列表筛成该类（再点取消）。分类恒按 Rk6 × 合成拥挤分位（五分量等权），与当前散点纵轴无关。点外圈=纵轴指标的20日位移（红走强/绿走弱）：${esc(ovAccelSpec().name)}；筛选时非目标的外圈压暗但仍可辨">`
      + Object.keys(OV_LABEL_COLOR).filter(k => k !== '数据不足')
        .map(k => {
          const n = cnt.get(k) || 0;
          return `<button type="button" class="ov-lg${ovCat === k ? ' on' : ''}" data-cat="${esc(k)}"`
            + `${n > 0 || ovCat === k ? '' : ' disabled'} title="${esc(OV_CAT_TIP[k])} · ${n} 个${n ? '，点击筛出' : '，当前无'}">`
            + `<i style="background:${OV_LABEL_COLOR[k]}"></i>${esc(OV_SHORT[k])}<em>${n}</em></button>`;
        }).join('')
      + `<button type="button" class="ov-lg${ovRs ? ' on' : ''}" data-rs90="1"${nRs > 0 || ovRs ? '' : ' disabled'}`
      + ` title="RS≥90 主线（rs20/63/252 截面分位等权合成，国盛双重验证口径；散点金圈）：点击只亮金圈点、其余压暗保留位置参照（再点取消） · ${nRs} 个">`
      + `<i style="background:#fcc419"></i>RS90<em>${nRs}</em></button>`
      + `</div>`;
  }

  function overviewQuad(rows, big = false) {
    const pts = rows.map(r => ({ r, p: ovCurPt(r) })).filter(o => o.p);
    // big=放大窗：viewBox 放大重画（等比放大点/线/字，屏占比更细的网格线）
    const W = big ? 680 : 300, H = big ? 400 : 214;
    const L = big ? 34 : 24, R = big ? 16 : 8, T = big ? 22 : 12, B = big ? 34 : 22;
    const pw = W - L - R, ph = H - T - B;
    const dotR = big ? 4 : 3.2;
    let X, Y, grid = '', axY, axX;
    if (ovMode === 'rs') {
      // 动态量程（含轨迹点），0 轴实线
      let lo = 0, hi = 0;
      for (const { p } of pts) { lo = Math.min(lo, p.x, p.y); hi = Math.max(hi, p.x, p.y); }
      if (ovTraj) for (const r of rows) for (const e of (r.traj_quad || [])) {
        const q = ovTrajPt(e);
        if (q) { lo = Math.min(lo, q.x, q.y); hi = Math.max(hi, q.x, q.y); }
      }
      const pad = Math.max((hi - lo) * 0.08, 0.5);
      lo -= pad; hi += pad;
      X = v => L + (v - lo) / (hi - lo) * pw;
      Y = v => T + (1 - (v - lo) / (hi - lo)) * ph;
      grid += `<line x1="${X(0).toFixed(1)}" y1="${T}" x2="${X(0).toFixed(1)}" y2="${T + ph}" class="ov-grid"/>`
        + `<line x1="${L}" y1="${Y(0).toFixed(1)}" x2="${L + pw}" y2="${Y(0).toFixed(1)}" class="ov-grid"/>`;
      axY = '↑RS252'; axX = 'RS63→（对000300）';
    } else {
      X = v => L + (v / 100) * pw;
      Y = v => T + (1 - v / 100) * ph;
      [50].forEach(v => {
        grid += `<line x1="${X(v).toFixed(1)}" y1="${T}" x2="${X(v).toFixed(1)}" y2="${T + ph}" class="ov-grid"/>`
          + `<line x1="${L}" y1="${Y(v).toFixed(1)}" x2="${L + pw}" y2="${Y(v).toFixed(1)}" class="ov-grid"/>`;
      });
      [20, 80].forEach(v => {
        grid += `<line x1="${X(v).toFixed(1)}" y1="${T}" x2="${X(v).toFixed(1)}" y2="${T + ph}" class="ov-grid dash"/>`
          + `<line x1="${L}" y1="${Y(v).toFixed(1)}" x2="${L + pw}" y2="${Y(v).toFixed(1)}" class="ov-grid dash"/>`;
      });
      axY = ovMode === 'comp' ? '↑RkC' : ovMode === 'ir' ? '↑IR' : '↑Rk6';
      axX = '合成拥挤→（五分量）';
    }
    let trajs = '';
    if (ovTraj) {
      trajs = rows.map(r => {
        const seq = (r.traj_quad || []).map(ovTrajPt).filter(Boolean);
        if (seq.length < 2) return '';
        const d = seq.map(q => `${X(q.x).toFixed(1)},${Y(q.y).toFixed(1)}`).join(' ');
        return `<polyline class="ov-traj" data-code="${esc(r.code)}" points="${d}"/>`;
      }).join('');
    }
    const dots = pts.map(({ r, p }) => {
      const x = X(p.x).toFixed(1), y = Y(p.y).toFixed(1);
      // 筛选时非目标的点压到 15% 透明度：散点留住位置参照，点的还是那一批
      const dim = (ovCat && ovLabel(r) !== ovCat) || (ovRs && !r.rs90);
      const op = dim ? ' opacity="0.15"' : '';
      // 动量加速度外圈：红=20日走强 / 绿=走弱（A股涨红跌绿），颜色引用 --up/--down，
      // 与下方表格 Δ20 列同色；口径跟纵轴走（见 OV_ACCEL），|Δ|≥strong 加粗，低于噪声门不画
      const acc = ovAccel(r);
      let ring = '';
      if (acc) {
        ring = `<circle cx="${x}" cy="${y}" r="${(dotR + 1.8).toFixed(1)}" fill="none" pointer-events="none"`
          + ` style="stroke:${acc.v > 0 ? 'var(--up)' : 'var(--down)'}"`
          + ` stroke-opacity="${ovRingOpacity(dim, acc.strong)}"`
          + ` stroke-width="${acc.strong ? 1.6 : 1}"/>`;
      }
      return ring
        + `<circle class="ov-dot${r.rs90 ? ' rs90' : ''}" data-code="${esc(r.code)}" data-ref="${esc(r.ref)}" data-name="${esc(r.name)}"`
        + ` cx="${x}" cy="${y}" r="${dotR}" fill="${ovColor(ovLabel(r))}"${op}>`
        + `<title>${esc(ovDotTip(r))}</title></circle>`;
    }).join('');
    return `<svg class="ov-quad${big ? ' ov-quad-big' : ''}" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet"`
      + (big ? ' style="width:100%;height:100%"' : ` width="100%" height="${H}"`) + '>'
      + `<rect x="${L}" y="${T}" width="${pw}" height="${ph}" class="ov-plot"/>` + grid + trajs + dots
      + `<text x="2" y="${T + 3}" class="ov-ax">${axY}</text>`
      + `<text x="${W - 2}" y="${H - 6}" class="ov-ax" text-anchor="end">${axX}</text>`
      + `</svg>`;
  }

  function renderOverview() {
    if (ovParent) return renderOvDrill();
    const body = M.ensurePanel();
    const tabs = M.tabsHtml();
    if (!overviewData) {
      body.innerHTML = tabs + (overviewLoading
        ? '<div class="dim">首次计算 128 个研究行业 / 5587 只成员的全市场广度约 50 秒（之后按交易日缓存，毫秒级）…</div>'
        : '<div class="dim">无数据</div>');
      M.bindTabs(body);
      return;
    }
    const d = overviewData;
    const inds = d.industries || [];
    const data = sortOverview(inds);
    const shown = data.filter(r => (!ovCat || ovLabel(r) === ovCat) && (!ovRs || r.rs90));
    const src = d.source || {};
    const quadN = data.filter(r => ovCurPt(r)).length;

    const long = (d.note_long || d.note || '') + `（散点 ${quadN}/${inds.length}）`
      + (d.stale_reason ? `\n陈旧原因：${d.stale_reason}` : '');
    // 陈旧/覆盖率后端一直在算，界面从没显示：基准 000300 落后一天时整片指标列会按
    // 新鲜度门禁置空，看着像坏了。把原因摊开，数值本身没错。
    const cov = d.coverage || {};
    const covShort = cov.full != null && cov.as_of != null && cov.as_of < cov.full;
    const flags = (d.stale
      ? `<span class="ov-stale" title="${esc(d.stale_reason || '本地日线未更新到最近交易日')}">`
        + `${d.session ? '盘中' : '陈旧'}</span>`
      : '')
      + (covShort
        ? ` <span class="dim" title="当日有 K 线的行业 ${cov.as_of}/${cov.full}：成交额占比的分母只含这些行业，覆盖率不足满覆盖日 90% 时该列不出数">${cov.as_of}/${cov.full}</span>`
        : '');
    const status = `<div class="dim heat-status" title="${esc(long)}">as-of ${esc(d.as_of)}${flags} · ${esc(src.sectors || '')}`
      + ` · ${esc(src.breadth || '')}`
      + ` · <span title="panels.js 版本戳（写死在代码里；index.html 的 ?v= 需与之一致）">js ${esc(TV_JS_BUILD)}</span>`
      + (TV_JS_MISMATCH
        ? ` · <span class="down" title="index.html 请求的是 ?v=${esc(TV_JS_MISMATCH)}，与脚本内的 ${esc(TV_JS_BUILD)} 不一致——多半是浏览器还在用缓存的旧 panels.js">js↔v 不一致</span>`
        : '')
      + ((ovCat || ovRs) ? `<span class="ov-filter">已筛${ovCat ? `「${esc(OV_SHORT[ovCat] || ovCat)}」` : ''}${ovRs ? '「RS90」' : ''} ${shown.length}/${data.length} · 点同类取消</span>` : '')
      + `</div>`;
    const legend = ovLegendHtml(data);

    const arrow = k => (overviewSort === k ? (overviewSortAsc ? '▲' : '▼') : '');
    const th = (key, label, r, tip) =>
      `<th class="ov-th ${r || ''}"${key ? ` data-sort="${key}"` : ''}${tip ? ` title="${esc(tip)}"` : ''}>${label}<span class="ov-arrow">${arrow(key)}</span></th>`;
    const head = `<thead><tr>${th('', '行业', 'ov-hname')}${th('day', '今', 'r')}${th('m6', '6M', 'r')}`
      + `${th('rank6', 'Rk6', 'r')}${th('rankc', '合成', 'r')}${th('d20', 'Δ20', 'r', 'Rk6 排名的 20 日位移（固定 Rk6 口径，不随散点模式切换；各模式外圈见左侧图例）')}${th('crowd', '拥挤', 'r')}${th('rs', 'RS', 'r')}${th('amt', '量', 'r')}`
      + `${th('ma20', 'MA20', 'r')}${th('nh', '新高', 'r')}${th('rps', 'RPS63', 'r', '成分股 RPS 广度（季窗）：板块内「63 日涨幅在全 A 截面分位 ≥90」的成分占比，北交所不进截面。衡量板块强是普涨还是几只权重拉的；月窗（20 日）见行 tooltip')}${th('hi', '距高', 'r')}</tr></thead>`;

    const rowHtml = r => {
      const m = r.mom || {}, b = r.breadth || {};
      const d20 = r.rank6_d20;
      const cx = ovCrowdX(r);
      const d20t = d20 == null ? '—'
        : (d20 > 0 ? '▲' : d20 < 0 ? '▼' : '') + Math.abs(d20).toFixed(0);
      const title = `${r.name} ${r.code}\n`
        + `1M ${signed(m['1M'])}  3M ${signed(m['3M'])}  6M ${signed(m['6M'])}  9M ${signed(m['9M'])}  12M ${signed(m['12M'])}\n`
        + `RS20 ${plain(r.rs20)}  RS63 ${plain(r.rs63)}  RS252 ${plain(r.rs252)}  连续跑赢 ${r.streak_rel == null ? '—' : r.streak_rel} 日\n`
        + `${ovCrowdLabel(r)} ${plain(cx, 0)} = 份额${plain(r.crowd_pct, 0)} 振幅${plain(r.crowd_vol_pct, 0)} β${plain(r.crowd_beta_pct, 0)} 量价${plain(r.crowd_corr_pct, 0)} 乖离${plain(r.crowd_bias_pct, 0)}\n`
        + `RS合成 ${plain(r.rs_comp, 0)}${r.rs90 ? ' ★主线(RS≥90)' : ''}  IR排名 ${plain(r.mom_rank_ir, 0)}\n`
        + `Rk6Δ20 ${d20t}  合成分Δ20 ${r.rankc_d20 == null ? '—' : (r.rankc_d20 > 0 ? '▲' : r.rankc_d20 < 0 ? '▼' : '') + Math.abs(r.rankc_d20).toFixed(0)}\n`
        + `距年低 ${signed(r.off_lo250)}  MA60上方 ${plain(b.above_ma60_pct, 0)}%\n`
        + `成分RPS≥90 季 ${plain(b.rps63_pct, 0)}% / 月 ${plain(b.rps20_pct, 0)}%（月高于季=近期新强，反之退潮）\n`
        + `有效样本 ${b.n_quoted == null ? '—' : b.n_quoted}/${b.n_total == null ? '—' : b.n_total}`
        + `  参与计算 ${r.hist_bars == null ? '—' : r.hist_bars} 根  ${ovLabel(r)}`
        + `\n单击 = 板块 K 线 · 双击 = 成分股`;
      return `<tr data-code="${esc(r.code)}" data-ref="${esc(r.ref)}" data-name="${esc(r.name)}" class="click" title="${esc(title)}">`
        + `<td class="ov-nm"><span class="ov-chip" style="background:${ovColor(ovLabel(r))}">${esc(OV_SHORT[ovLabel(r)] || '')}</span>`
        + `<div class="ov-txt"><div class="scan-nm">${esc(r.name)}</div><div class="scan-code">${esc(r.code)}</div></div></td>`
        + `<td class="r ov-num ${numCls(r.day_ret)}">${signed(r.day_ret)}</td>`
        + `<td class="r ov-num ${numCls(m['6M'])}">${signed(m['6M'])}</td>`
        + bcell(r.mom_rank6)
        + bcell(r.mom_rank_c)
        + `<td class="r ov-num ${numCls(d20)}">${d20t}</td>`
        + bcell(r.crowd_comp != null ? r.crowd_comp : r.crowd_pct)
        + `<td class="r ov-num">${r.rs_comp == null ? '—' : (r.rs90 ? '<b style="color:#fcc419">' + r.rs_comp.toFixed(0) + '★</b>' : r.rs_comp.toFixed(0))}</td>`
        + `<td class="r ov-num">${r.amt_x == null ? '—' : r.amt_x.toFixed(2) + '×'}</td>`
        + bcell(b.above_ma20_pct)
        + `<td class="r ov-num">${b.nh60_pct == null ? '—' : b.nh60_pct.toFixed(0)}</td>`
        + bcell(b.rps63_pct)
        + `<td class="r ov-num ${numCls(r.off_hi250)}">${signed(r.off_hi250)}</td>`
        + `</tr>`;
    };

    const modeBar = `<div class="ov-modes">` + ovModeBtns('')
      + `<button class="mini" id="ov-big" title="把散点放大到独立大窗查看（当前模式：${OV_MODES[ovMode]}，窗内可切模式 / 开轨迹）。点窗口外空白或按 Esc 关闭">⤢ 放大</button></div>`;

    body.innerHTML = tabs + status
      + modeBar + overviewQuad(data) + legend
      + `<div class="ov-scroll"><table class="grid ov-table">${head}<tbody>`
      + shown.map(rowHtml).join('')
      + `</tbody></table></div>`;
    bindOverviewControls(body);
  }

  // —— 板块总览下钻：与行业热度点击行业后的行为一致（面板变成份股表 + 右侧开板块图）——
  // 成份股数据直接复用热度载荷的 stocks 映射。注意键系不同：总览行业是 881 码，
  // 热度 stocks 以目录节点的 ref（X 码）为键，member.industry 亦以 X 码为前缀——
  // 所以下钻用行上的 data-ref 查表，data-code 只用于 openBoard。
  function ovDrillStocks() {
    return (M.getHeat() && M.getHeat().stocks && M.getHeat().stocks[ovParent]) || null;
  }

  function renderOvDrill() {
    const body = M.ensurePanel();
    M.renderDrill(body, {
      title: ovParentName || ovParent,
      rows: ovDrillStocks() || [],
      loading: ovDrillLoading,
      backId: 'ov-back', backLabel: '‹ 返回总览',
      onBack: () => { ovParent = ''; ovParentName = ''; renderOverview(); },
    });
  }

  async function drillOverview(code, name, ref) {
    // 总览的 881 码只用于开板块图；热度 stocks 映射的键是目录节点的 ref（X 码）
    ovParent = ref || '';
    ovParentName = name;
    const have = !!(ovParent && ovDrillStocks());
    ovDrillLoading = !have;
    if (M.tab === 'overview') renderOverview();
    M.pickBoard(code, name);   // 双击路径上主图已由单击切过来了，这里是空转；保留是为了它自成闭环
    if (!have) {
      try {
        const j = await TV.api.heat();
        if (j && j.stocks) { M.setHeat(j); }
      } catch (_) {}
      ovDrillLoading = false;
      if (ovParent === ref && M.tab === 'overview') renderOverview();
    }
  }

  // —— 散点放大窗：全屏遮罩 + 大幅重画（与侧栏共享 ovMode/ovTraj，双向同步）——
  let ovBigMask = null;
  let ovBigHold = null;   // 大窗散点的单击挂起（给 dblclick 让位，见 renderOvBig）
  function setOvMode(m) {
    ovMode = m;
    try { localStorage.setItem(OV_MODE_KEY, m); } catch (_) {}
    if (M.tab === 'overview') renderOverview();
    renderOvBig();
  }
  function setOvTraj() {
    ovTraj = !ovTraj;
    try { localStorage.setItem('tv.ov.traj', ovTraj ? '1' : '0'); } catch (_) {}
    if (M.tab === 'overview') renderOverview();
    renderOvBig();
  }
  // 点图例 = 按类筛下面的列表；再点同一个取消。大窗里的图例只管散点压暗（那没有列表）。
  function setOvCat(k) {
    ovCat = ovCat === k ? '' : k;
    if (M.tab === 'overview') renderOverview();
    renderOvBig();
  }
  // RS90 金圈筛选：与 ovCat 独立开关、可叠加
  function setOvRs() {
    ovRs = !ovRs;
    if (M.tab === 'overview') renderOverview();
    renderOvBig();
  }
  function bindOvLegend(scope) {
    scope.querySelectorAll('button[data-cat]').forEach(b => { b.onclick = () => setOvCat(b.dataset.cat); });
    scope.querySelectorAll('button[data-rs90]').forEach(b => { b.onclick = setOvRs; });
  }
  function renderOvBig() {
    if (!ovBigMask || !overviewData) return;
    const d = overviewData;
    const data = sortOverview(d.industries || []);
    const n = data.filter(r => ovCurPt(r)).length;
    const legend = ovLegendHtml(data);
    ovBigMask.innerHTML = `<div class="ov-big"><div class="ov-big-head">`
      + `<b>板块散点 · ${OV_MODES[ovMode]}<span class="dim">（${n}/${data.length}）</span></b>` + legend
      + `<div class="ov-modes">${ovModeBtns('-b')}`
      + `<button class="mini" id="ov-big-close" title="关闭（Esc）">×</button></div></div>`
      + `<div class="ov-big-body">${overviewQuad(data, true)}</div></div>`;
    ovBigMask.querySelectorAll('button[data-ovmode]').forEach(b => { b.onclick = () => setOvMode(b.dataset.ovmode); });
    bindOvLegend(ovBigMask);
    const tb = ovBigMask.querySelector('#ov-traj-b');
    if (tb) tb.onclick = setOvTraj;
    ovBigMask.querySelector('#ov-big-close').onclick = closeOvBig;
    // 大窗里的点沿用同一手势（单击开图 / 双击下钻），但单击必须关窗——元素随之消失，
    // 第二记 click 就落不到点上、dblclick 也不会发。所以挂起 240ms：等不来 dblclick 才关窗开图。
    clearTimeout(ovBigHold);
    ovBigHold = null;
    ovBigMask.querySelectorAll('circle.ov-dot').forEach(c => {
      c.addEventListener('click', () => {
        clearTimeout(ovBigHold);
        ovBigHold = setTimeout(() => {
          ovBigHold = null;
          closeOvBig();
          M.pickBoard(c.dataset.code, c.dataset.name);
        }, 240);
      });
      c.addEventListener('dblclick', () => {
        clearTimeout(ovBigHold);
        ovBigHold = null;
        drillOverview(c.dataset.code, c.dataset.name, c.dataset.ref);
        closeOvBig();
      });
    });
    if (ovTraj) {
      ovBigMask.querySelectorAll('circle.ov-dot').forEach(c => {
        c.addEventListener('mouseenter', () => {
          ovBigMask.querySelectorAll('polyline.ov-traj.hi').forEach(p => p.classList.remove('hi'));
          const p = ovBigMask.querySelector(`polyline.ov-traj[data-code="${c.dataset.code.replace(/["\\]/g, '\\$&')}"]`);
          if (p) p.classList.add('hi');
        });
      });
    }
  }
  function openOvBig() {
    if (!overviewData) return;
    if (!ovBigMask) {
      ovBigMask = document.createElement('div');
      ovBigMask.className = 'ov-big-mask';
      ovBigMask.addEventListener('mousedown', e => { if (e.target === ovBigMask) closeOvBig(); });
      document.body.appendChild(ovBigMask);
      document.addEventListener('keydown', ovBigKey);
    }
    renderOvBig();
  }
  function closeOvBig() {
    clearTimeout(ovBigHold);
    ovBigHold = null;
    if (ovBigMask) { ovBigMask.remove(); ovBigMask = null; }
    document.removeEventListener('keydown', ovBigKey);
  }
  function ovBigKey(e) { if (e.key === 'Escape') closeOvBig(); }

  function bindOverviewControls(body) {
    M.bindTabs(body);
    body.querySelectorAll('button[data-ovmode]').forEach(b => { b.onclick = () => setOvMode(b.dataset.ovmode); });
    bindOvLegend(body);
    const trajBtn = body.querySelector('#ov-traj');
    if (trajBtn) trajBtn.onclick = setOvTraj;
    const bigBtn = body.querySelector('#ov-big');
    if (bigBtn) bigBtn.onclick = openOvBig;
    if (ovTraj) {
      body.querySelectorAll('circle.ov-dot').forEach(c => {
        c.addEventListener('mouseenter', () => {
          body.querySelectorAll('polyline.ov-traj.hi').forEach(p => p.classList.remove('hi'));
          const p = body.querySelector(`polyline.ov-traj[data-code="${c.dataset.code.replace(/["\\]/g, '\\$&')}"]`);
          if (p) p.classList.add('hi');
        });
      });
    }
    body.querySelectorAll('th[data-sort]').forEach(el => {
      el.onclick = () => {
        const key = el.dataset.sort;
        if (overviewSort === key) overviewSortAsc = !overviewSortAsc;
        else { overviewSort = key; overviewSortAsc = false; }
        renderOverview();
      };
    });
    const open = (code, name, ref) => drillOverview(code, name, ref);
    body.querySelectorAll('tr[data-code]').forEach(tr => {
      M.bindBoardDbl(tr, tr.dataset.code, tr.dataset.name,
        () => open(tr.dataset.code, tr.dataset.name, tr.dataset.ref));
    });
    body.querySelectorAll('circle.ov-dot').forEach(c => {
      M.bindBoardDbl(c, c.dataset.code, c.dataset.name,
        () => open(c.dataset.code, c.dataset.name, c.dataset.ref));
    });
  }

  async function loadOverview(force = false) {
    if (overviewBuilt && !force) { renderOverview(); return; }
    if (overviewLoading && !force) return;
    const body = M.ensurePanel();
    if (!overviewData) {
      body.innerHTML = M.tabsHtml()
        + '<div class="dim">首次计算 128 个研究行业 / 5587 只成员的全市场广度约 50 秒（之后按交易日缓存，毫秒级）…</div>';
      M.bindTabs(body);
    }
    overviewLoading = true;
    overviewError = '';
    try {
      const j = await TV.api.sectorOverview(!!force);
      overviewData = j;
      overviewBuilt = true;
      overviewLoading = false;
      if (M.tab === 'overview') renderOverview();
    } catch (e) {
      overviewLoading = false;
      overviewError = e.message;
      if (M.tab === 'overview') {
        body.innerHTML = M.tabsHtml() + `<div class="down">${esc(e.message)}</div>`;
        M.bindTabs(body);
      }
    }
  }
  TV.overview = { render: renderOverview, load: loadOverview };
})();
