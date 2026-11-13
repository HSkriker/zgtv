// PA2 · SMC（Smart Money Concepts / ICT）算法层。
// 逐段移植自 smart-money-concepts（Hskriker fork 0.0.27）的 smartmoneyconcepts/smc.py：
//   swing_highs_lows / bos_choch / fvg / ob / liquidity / retracements / previous_high_low
// 上游链：joshyattridge/smartmoneyconcepts（原作，MIT，© 2020 NeuralNine）
//   → Hskriker fork 0.0.27 → 本文件；署名见 THIRD-PARTY-NOTICES.md §2.1。
// 口径与上游一致；与 zg pa2-layer.js（基于上游 0.0.25）的差异见
//   docs/模块来源与使用说明.md §1.4（zg 照抄了 numpy 负索引回绕，
//   fork 已改为显式取 0，本移植取 fork 口径）。
// 纯计算，无网络、无 DOM 依赖。
(function () {
  const TV = (window.TV = window.TV || {});

  const DEFAULT_SWING = 10;   // compute() 的兜底值；图层按周期档用 SWING_DEFAULTS
  const SWING_OPTIONS = [5, 10, 20, 50];
  // 各周期档默认摆动长度（单位 = 当前档 K 线根数）：低周期压噪声，高周期防根数不足。
  // 1/5 分钟 50；15/30 分钟 20；60 分钟/日/周 10；月/季/年 5。
  const SWING_DEFAULTS = {
    '1': 50, '5': 50, '15': 20, '30': 20, '60': 10,
    day: 10, week: 10, month: 5, quarter: 5, year: 5,
  };
  const MIN_GAP_ATR = 0.15;   // 窄 gap FVG 过滤阈值（×ATR60），见 dropNarrowFvg
  const ATR_LEN = 60;
  const ATR_MIN_SAMPLES = 14;

  // numpy float32 数组写入语义（Python 侧对应 np.float32(...)）
  const f32 = Math.fround;

  /* ① swing_highs_lows：±L 对称窗口极值 + 同向折叠 + 首尾人造点
     上游：high[j] == rolling(2L).max()(j)，窗口覆盖 [j-L+1, j+L]，
     有效范围 j ∈ [2L-1, n-1-L]（shift 后首尾缺值，rolling 需满窗）。 */
  function swingHighsLows(h, l, swingLength) {
    const n = h.length;
    const L = swingLength;
    const hl = new Int8Array(n);
    const level = new Float64Array(n);
    if (n < 2 * L + 1) return { hl, level };

    for (let i = 2 * L - 1; i + L < n; i++) {
      let mx = -Infinity, mn = Infinity;
      for (let j = i - (L - 1); j <= i + L; j++) {
        if (h[j] > mx) mx = h[j];
        if (l[j] < mn) mn = l[j];
      }
      if (h[i] === mx) hl[i] = 1;
      else if (l[i] === mn) hl[i] = -1;
    }

    // 同向相邻摆动点折叠：每轮同时删除，直到没有同向相邻为止
    for (;;) {
      const pos = [];
      for (let i = 0; i < n; i++) if (hl[i]) pos.push(i);
      if (pos.length < 2) break;
      const kill = new Uint8Array(pos.length);
      let any = false;
      for (let k = 0; k + 1 < pos.length; k++) {
        const a = pos[k], b = pos[k + 1];
        if (hl[a] === 1 && hl[b] === 1) {
          if (h[a] < h[b]) { kill[k] = 1; any = true; }
          else { kill[k + 1] = 1; any = true; }
        } else if (hl[a] === -1 && hl[b] === -1) {
          if (l[a] > l[b]) { kill[k] = 1; any = true; }
          else { kill[k + 1] = 1; any = true; }
        }
      }
      if (!any) break;
      for (let k = 0; k < pos.length; k++) if (kill[k]) hl[pos[k]] = 0;
    }

    // 首尾人造点：上游写的是数组下标 0 与 n-1（检测区间不含这两个下标）
    let first = -1, last = -1;
    for (let i = 0; i < n; i++) if (hl[i]) { first = i; break; }
    for (let i = n - 1; i >= 0; i--) if (hl[i]) { last = i; break; }
    if (first >= 0) {
      if (hl[first] === 1) hl[0] = -1;
      else hl[0] = 1;
      if (hl[last] === -1) hl[n - 1] = 1;
      else hl[n - 1] = -1;
    }

    for (let i = 0; i < n; i++) {
      if (!hl[i]) continue;
      level[i] = hl[i] === 1 ? h[i] : l[i];
    }
    return { hl, level };
  }

  /* ② bos_choch：连续 4 个摆动点的结构判定 + 被破坏 K 线定位
     上游把 BOS/CHoCH 记在倒数第二个摆动点上，Level 取倒数第三个摆动的价位。 */
  function bosChoch(h, l, c, hl, level, closeBreak) {
    const n = hl.length;
    const bos = new Int8Array(n), choch = new Int8Array(n);
    const lev = new Float64Array(n);
    const levelOrder = [], typeOrder = [], lastPos = [];

    for (let i = 0; i < n; i++) {
      if (!hl[i]) continue;
      levelOrder.push(level[i]);
      typeOrder.push(hl[i]);
      const m = levelOrder.length;
      if (m >= 4) {
        const p = lastPos[lastPos.length - 2];
        const L4 = levelOrder[m - 4], L3 = levelOrder[m - 3], L2 = levelOrder[m - 2], L1 = levelOrder[m - 1];
        const t4 = typeOrder[m - 4], t3 = typeOrder[m - 3], t2 = typeOrder[m - 2], t1 = typeOrder[m - 1];
        const bullType = t4 === -1 && t3 === 1 && t2 === -1 && t1 === 1;
        const bearType = t4 === 1 && t3 === -1 && t2 === 1 && t1 === -1;

        // 看涨 BOS：HL→LH→HL→HH 且 level_order[-4] < [-2] < [-3] < [-1]
        bos[p] = (bullType && L4 < L2 && L2 < L3 && L3 < L1) ? 1 : 0;
        lev[p] = bos[p] !== 0 ? f32(L3) : 0;
        // 看跌 BOS
        bos[p] = (bearType && L4 > L2 && L2 > L3 && L3 > L1) ? -1 : bos[p];
        lev[p] = bos[p] !== 0 ? f32(L3) : 0;
        // 看涨 CHoCH
        choch[p] = (bullType && L1 > L3 && L3 > L4 && L4 > L2) ? 1 : 0;
        lev[p] = choch[p] !== 0 ? f32(L3) : lev[p];
        // 看跌 CHoCH（上游 else 分支保留原值，不是清零）
        choch[p] = (bearType && L1 < L3 && L3 < L4 && L4 < L2) ? -1 : choch[p];
        lev[p] = choch[p] !== 0 ? f32(L3) : lev[p];
      }
      lastPos.push(i);
    }

    // 找被破坏的那根 K 线；后出现的破坏会抹掉更早且同区间结束的未破坏结构
    const broken = new Int32Array(n);
    const cand0 = [];
    for (let i = 0; i < n; i++) if (bos[i] !== 0 || choch[i] !== 0) cand0.push(i);
    for (const i of cand0) {
      const up = bos[i] === 1 || choch[i] === 1;
      const down = bos[i] === -1 || choch[i] === -1;
      if (!up && !down) continue;
      const src = closeBreak ? c : (up ? h : l);
      let j = -1;
      for (let k = i + 2; k < n; k++) {
        if (up ? src[k] > lev[i] : src[k] < lev[i]) { j = k; break; }
      }
      if (j < 0) continue;
      broken[i] = j;
      const inner = [];
      for (let k = 0; k < n; k++) if (bos[k] !== 0 || choch[k] !== 0) inner.push(k);
      for (const k of inner) {
        if (k < i && broken[k] >= j) { bos[k] = 0; choch[k] = 0; lev[k] = 0; }
      }
    }
    // 未被破坏的清零
    for (let i = 0; i < n; i++) {
      if ((bos[i] !== 0 || choch[i] !== 0) && broken[i] === 0) { bos[i] = 0; choch[i] = 0; lev[i] = 0; }
    }
    return { bos, choch, level: lev, broken };
  }

  /* ③ fvg：三根 K 线的 Fair Value Gap + 回补 K 线下标 */
  function fairValueGaps(o, h, l, c) {
    const n = o.length;
    const fvg = new Int8Array(n);
    const top = new Float64Array(n), bottom = new Float64Array(n);
    const mitigated = new Int32Array(n);
    for (let i = 0; i < n; i++) {
      const bull = h[i - 1] < l[i + 1] && c[i] > o[i];
      const bear = l[i - 1] > h[i + 1] && c[i] < o[i];
      if (!bull && !bear) continue;
      fvg[i] = bull ? 1 : -1;
      top[i] = bull ? l[i + 1] : l[i - 1];
      bottom[i] = bull ? h[i - 1] : h[i + 1];
    }
    for (let i = 0; i < n; i++) {
      if (!fvg[i]) continue;
      for (let j = i + 2; j < n; j++) {
        if (fvg[i] === 1 ? l[j] <= top[i] : h[j] >= bottom[i]) { mitigated[i] = j; break; }
      }
    }
    return { fvg, top, bottom, mitigated };
  }

  /* ④ ob：订单块（摆动高/低被收盘价突破后，回找极值 K 线作区块）
     fork 0.0.27 的成交量口径：越界取 0（不再沿用 numpy 负索引回绕）。 */
  function orderBlocks(o, h, l, c, vol, hl, closeMitigation) {
    const n = hl.length;
    const ob = new Int8Array(n);
    const top = new Float64Array(n), bottom = new Float64Array(n);
    const obVolume = new Float64Array(n), pct = new Float64Array(n);
    const mitigated = new Int32Array(n);
    const crossed = new Uint8Array(n), breaker = new Uint8Array(n);

    const swingHi = [], swingLo = [];
    for (let i = 0; i < n; i++) { if (hl[i] === 1) swingHi.push(i); else if (hl[i] === -1) swingLo.push(i); }
    const lastBefore = (arr, x) => {
      let lo = 0, hi = arr.length;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (arr[mid] < x) lo = mid + 1; else hi = mid; }
      return lo > 0 ? arr[lo - 1] : -1;
    };
    const volAt = k => (k >= 0 && k < n ? vol[k] : 0);

    const activeBull = [], activeBear = [];
    for (let ci = 0; ci < n; ci++) {
      for (let a = activeBull.length - 1; a >= 0; a--) {
        const idx = activeBull[a];
        if (breaker[idx]) {
          if (h[ci] > top[idx]) {
            ob[idx] = 0; top[idx] = 0; bottom[idx] = 0; obVolume[idx] = 0; pct[idx] = 0; mitigated[idx] = 0;
            activeBull.splice(a, 1);
          }
        } else {
          const hit = closeMitigation
            ? Math.min(o[ci], c[ci]) < bottom[idx]
            : l[ci] < bottom[idx];
          if (hit) { breaker[idx] = 1; mitigated[idx] = ci - 1; }
        }
      }
      const lth = lastBefore(swingHi, ci);
      if (lth >= 0 && c[ci] > h[lth] && !crossed[lth]) {
        crossed[lth] = 1;
        let obIdx = ci - 1, obBtm = h[obIdx], obTop = l[obIdx];
        if (ci - lth > 1) {
          const start = lth + 1, end = ci;
          if (end > start) {
            let best = Infinity, cand = -1;
            for (let k = start; k < end; k++) if (l[k] <= best) { best = l[k]; cand = k; }
            if (cand >= 0) { obIdx = cand; obBtm = l[cand]; obTop = h[cand]; }
          }
        }
        ob[obIdx] = 1;
        top[obIdx] = obTop;
        bottom[obIdx] = obBtm;
        const volCur = volAt(ci), volPrev1 = volAt(ci - 1), volPrev2 = volAt(ci - 2);
        obVolume[obIdx] = f32(volCur + volPrev1 + volPrev2);
        const loV = f32(volPrev2), hiV = f32(volCur + volPrev1);
        const mx = Math.max(hiV, loV);
        pct[obIdx] = mx === 0 ? 100 : f32(f32(Math.min(hiV, loV) / mx) * 100);
        activeBull.push(obIdx);
      }
    }

    for (let ci = 0; ci < n; ci++) {
      for (let a = activeBear.length - 1; a >= 0; a--) {
        const idx = activeBear[a];
        if (breaker[idx]) {
          if (l[ci] < bottom[idx]) {
            ob[idx] = 0; top[idx] = 0; bottom[idx] = 0; obVolume[idx] = 0; pct[idx] = 0; mitigated[idx] = 0;
            activeBear.splice(a, 1);
          }
        } else {
          const hit = closeMitigation
            ? Math.max(o[ci], c[ci]) > top[idx]
            : h[ci] > top[idx];
          if (hit) { breaker[idx] = 1; mitigated[idx] = ci; }
        }
      }
      const lth = lastBefore(swingLo, ci);
      if (lth >= 0 && c[ci] < l[lth] && !crossed[lth]) {
        crossed[lth] = 1;
        let obIdx = ci - 1, obTop = h[obIdx], obBtm = l[obIdx];
        if (ci - lth > 1) {
          const start = lth + 1, end = ci;
          if (end > start) {
            let best = -Infinity, cand = -1;
            for (let k = start; k < end; k++) if (h[k] >= best) { best = h[k]; cand = k; }
            if (cand >= 0) { obIdx = cand; obTop = h[cand]; obBtm = l[cand]; }
          }
        }
        ob[obIdx] = -1;
        top[obIdx] = obTop;
        bottom[obIdx] = obBtm;
        const volCur = volAt(ci), volPrev1 = volAt(ci - 1), volPrev2 = volAt(ci - 2);
        obVolume[obIdx] = f32(volCur + volPrev1 + volPrev2);
        const loV = f32(volCur + volPrev1), hiV = f32(volPrev2);
        const mx = Math.max(hiV, loV);
        pct[obIdx] = mx === 0 ? 100 : f32(f32(Math.min(hiV, loV) / mx) * 100);
        activeBear.push(obIdx);
      }
    }
    return { ob, top, bottom, obVolume, pct, mitigated };
  }

  /* ⑤ liquidity：同一价位附近的多个摆动点（等高等低）+ 扫单 K 线 */
  function liquidity(hl, level, h, l, rangePercent) {
    const n = hl.length;
    let hiMax = -Infinity, loMin = Infinity;
    for (let i = 0; i < n; i++) { if (h[i] > hiMax) hiMax = h[i]; if (l[i] < loMin) loMin = l[i]; }
    const pipRange = (hiMax - loMin) * rangePercent;

    const used = new Int8Array(n);
    const liq = new Int8Array(n), liqLevel = new Float64Array(n);
    const liqEnd = new Int32Array(n), liqSwept = new Int32Array(n);

    for (const side of [1, -1]) {
      const idxs = [];
      for (let i = 0; i < n; i++) if (hl[i] === side) idxs.push(i);
      for (const i of idxs) {
        if (hl[i] !== side || used[i]) continue;
        const lv = level[i];
        const rLow = lv - pipRange, rHigh = lv + pipRange;
        const group = [lv];
        let groupEnd = i, swept = 0;
        for (let j = i + 1; j < n; j++) {
          if (side === 1 ? h[j] >= rHigh : l[j] <= rLow) { swept = j; break; }
        }
        for (const j of idxs) {
          if (j <= i) continue;
          if (swept && j >= swept) break;
          if (hl[j] === side && !used[j] && level[j] >= rLow && level[j] <= rHigh) {
            group.push(level[j]);
            groupEnd = j;
            used[j] = 1;
          }
        }
        if (group.length > 1) {
          liq[i] = side;
          liqLevel[i] = f32(group.reduce((a, b) => a + b, 0) / group.length);
          liqEnd[i] = groupEnd;
          liqSwept[i] = swept;
        }
      }
    }
    return { liq, level: liqLevel, end: liqEnd, swept: liqSwept };
  }

  /* ⑥ retracements：当前回撤 / 最深回撤百分比（上游 shift 后清空前 3 段方向变化） */
  function retracements(hl, level, h, l) {
    const n = hl.length;
    const dir = new Int8Array(n);
    const cur = new Float64Array(n), deep = new Float64Array(n);
    let top = 0, bottom = 0;
    for (let i = 0; i < n; i++) {
      if (hl[i] === 1) { dir[i] = 1; top = level[i]; }
      else if (hl[i] === -1) { dir[i] = -1; bottom = level[i]; }
      else dir[i] = i > 0 ? dir[i - 1] : 0;

      const prevDir = i > 0 ? dir[i - 1] : 0;
      if (prevDir === 1) {
        const divisor = top - bottom;
        cur[i] = divisor !== 0 ? Math.round((100 - ((l[i] - bottom) / divisor) * 100) * 10) / 10 : 0;
        const prevDeep = i > 0 && prevDir === 1 ? deep[i - 1] : 0;
        deep[i] = Math.max(prevDeep, cur[i]);
      }
      if (dir[i] === -1) {
        const divisor = bottom - top;
        cur[i] = divisor !== 0 ? Math.round((100 - ((h[i] - top) / divisor) * 100) * 10) / 10 : 0;
        const prevDeep = i > 0 && prevDir === -1 ? deep[i - 1] : 0;
        deep[i] = Math.max(prevDeep, cur[i]);
      }
    }
    // np.roll(arr, 1)：整体右移一位，末位绕到首位
    const roll = a => {
      const out = new Float64Array(n);
      out[0] = a[n - 1];
      for (let i = 1; i < n; i++) out[i] = a[i - 1];
      return out;
    };
    const rDir = new Int8Array(n);
    {
      const rd = roll(dir);
      for (let i = 0; i < n; i++) rDir[i] = rd[i];
    }
    const rCur = roll(cur), rDeep = roll(deep);

    // 清除前 3 次方向变化之前的回撤（上游：数据不足导致算错）
    let changes = 0;
    for (let i = 0; i < n; i++) {
      if (i + 1 === n) break;
      if (rDir[i] !== rDir[i + 1]) changes++;
      rDir[i] = 0; rCur[i] = 0; rDeep[i] = 0;
      if (changes === 3) {
        rDir[i + 1] = 0; rCur[i + 1] = 0; rDeep[i + 1] = 0;
        break;
      }
    }
    return { dir: rDir, cur: rCur, deep: rDeep };
  }

  /* ⑦ previous_high_low：上一周期高低点 + 是否破位
     上游按 time_frame 重采样；此处按 bars 的日期串分桶（'D'/'W'/'M'）。 */
  function periodKey(dateStr, tf) {
    const y = +dateStr.slice(0, 4), m = +dateStr.slice(5, 7), d = +dateStr.slice(8, 10);
    if (tf === 'M') return `${y}-${String(m).padStart(2, '0')}`;
    if (tf === 'W') {
      const dt = new Date(Date.UTC(y, m - 1, d));
      const day = (dt.getUTCDay() + 6) % 7;           // 周一=0
      dt.setUTCDate(dt.getUTCDate() - day + 3);       // 该周周四
      const isoY = dt.getUTCFullYear();
      const jan1 = Date.UTC(isoY, 0, 1);
      const week = Math.floor((dt.getTime() - jan1) / 604800000) + 1;
      return `${isoY}-W${String(week).padStart(2, '0')}`;
    }
    return dateStr.slice(0, 10);
  }

  function previousHighLow(bars, tf) {
    const n = bars.length;
    // 分钟档 time 是北京时间真实 UTC 纪元秒，先归一成可比较的本地串；日线及以上本就是日期串
    const tOf = b => typeof b.time === 'number'
      ? new Date((b.time + 8 * 3600) * 1000).toISOString().slice(0, 16).replace('T', ' ')
      : String(b.time || '');
    const groups = [];       // {key, start, high, low}
    const map = new Map();
    for (let i = 0; i < n; i++) {
      const ts = tOf(bars[i]);
      const key = periodKey(ts.slice(0, 10), tf);
      let g = map.get(key);
      if (!g) { g = { key, start: ts, high: -Infinity, low: Infinity }; map.set(key, g); groups.push(g); }
      if (bars[i].high > g.high) g.high = bars[i].high;
      if (bars[i].low < g.low) g.low = bars[i].low;
    }
    const prevHigh = new Float64Array(n).fill(NaN);
    const prevLow = new Float64Array(n).fill(NaN);
    const brokenHigh = new Int8Array(n);
    const brokenLow = new Int8Array(n);
    if (groups.length < 2) return { prevHigh, prevLow, brokenHigh, brokenLow };

    // 每根 K 线归属的「上一周期」：start 严格早于该 K 线时间的周期里取倒数第二个
    const gStart = groups.map(g => g.start);
    const groupOf = new Int32Array(n);
    {
      let g = 0;
      for (let i = 0; i < n; i++) {
        while (g + 1 < groups.length && gStart[g + 1] <= tOf(bars[i])) g++;
        groupOf[i] = g;
      }
    }
    const periodsBefore = new Int32Array(n);
    for (let i = 0; i < n; i++) {
      let c = 0;
      while (c < groups.length && gStart[c] < tOf(bars[i])) c++;
      periodsBefore[i] = c;
    }
    const prevIdx = new Int32Array(n);
    for (let i = 0; i < n; i++) prevIdx[i] = periodsBefore[i] - 2;

    // 按「上一周期」分组，组内累计最高/最低
    let gid = 0;
    const gidOf = new Int32Array(n);
    for (let i = 0; i < n; i++) {
      if (i > 0 && prevIdx[i] !== prevIdx[i - 1]) gid++;
      gidOf[i] = gid;
    }
    const cummax = new Float64Array(n), cummin = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      cummax[i] = bars[i].high; cummin[i] = bars[i].low;
      if (i > 0 && gidOf[i] === gidOf[i - 1]) {
        if (cummax[i - 1] > cummax[i]) cummax[i] = cummax[i - 1];
        if (cummin[i - 1] < cummin[i]) cummin[i] = cummin[i - 1];
      }
    }
    for (let i = 0; i < n; i++) {
      if (periodsBefore[i] <= 1) continue;
      const gi = prevIdx[i];
      if (gi < 0 || gi >= groups.length) continue;
      prevHigh[i] = f32(groups[gi].high);
      prevLow[i] = f32(groups[gi].low);
      if (cummax[i] > prevHigh[i]) brokenHigh[i] = 1;
      if (cummin[i] < prevLow[i]) brokenLow[i] = 1;
    }
    return { prevHigh, prevLow, brokenHigh, brokenLow };
  }

  /* 主计算：bars → 结构化结果 */
  function compute(bars, opts) {
    const o = opts || {};
    const swingLen = o.swingLength || DEFAULT_SWING;
    const closeBreak = o.closeBreak !== false;
    const rangePercent = o.rangePercent != null ? o.rangePercent : 0.01;
    const closeMitigation = !!o.closeMitigation;
    const n = bars.length;
    if (!n) return { ok: false, reason: '无K线' };

    const O = new Float64Array(n), H = new Float64Array(n), L = new Float64Array(n),
      C = new Float64Array(n), V = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      const b = bars[i];
      O[i] = +b.open; H[i] = +b.high; L[i] = +b.low; C[i] = +b.close;
      V[i] = Number.isFinite(+b.vol) ? +b.vol : (+b.volume || 0);
    }
    for (let i = 0; i < n; i++) {
      if (![O[i], H[i], L[i], C[i]].every(Number.isFinite)) return { ok: false, reason: 'K线数据含无效值' };
    }
    if (n < 2 * swingLen + 5) return { ok: false, reason: `K线不足 ${2 * swingLen + 5} 根（摆动长度 ${swingLen}）` };

    const sw = swingHighsLows(H, L, swingLen);
    const bc = bosChoch(H, L, C, sw.hl, sw.level, closeBreak);
    const fv = fairValueGaps(O, H, L, C);
    const ob = orderBlocks(O, H, L, C, V, sw.hl, closeMitigation);
    const lq = liquidity(sw.hl, sw.level, H, L, rangePercent);
    const rt = retracements(sw.hl, sw.level, H, L);
    const phl = previousHighLow(bars, o.timeFrame || 'W');

    // 结构化（只保留非空项，便于渲染与面板统计）
    const swings = [], bosList = [], fvgList = [], obList = [], liqList = [];
    for (let i = 0; i < n; i++) {
      if (sw.hl[i]) swings.push({ idx: i, kind: sw.hl[i], level: sw.level[i], time: bars[i].time });
      if (bc.bos[i] || bc.choch[i]) {
        bosList.push({
          idx: i, time: bars[i].time,
          kind: bc.bos[i] ? 'bos' : 'choch',
          dir: bc.bos[i] || bc.choch[i],
          level: bc.level[i],
          brokenIdx: bc.broken[i],
          brokenTime: bc.broken[i] ? bars[bc.broken[i]].time : null,
        });
      }
      if (fv.fvg[i]) fvgList.push({
        idx: i, time: bars[i].time, dir: fv.fvg[i], top: fv.top[i], bottom: fv.bottom[i],
        mitigatedIdx: fv.mitigated[i] || null,
        mitigatedTime: fv.mitigated[i] ? bars[fv.mitigated[i]].time : null,
      });
      if (ob.ob[i]) obList.push({
        idx: i, time: bars[i].time, dir: ob.ob[i], top: ob.top[i], bottom: ob.bottom[i],
        volume: ob.obVolume[i], pct: ob.pct[i], mitigatedIdx: ob.mitigated[i] || null,
      });
      if (lq.liq[i]) liqList.push({
        idx: i, time: bars[i].time, dir: lq.liq[i], level: lq.level[i],
        endIdx: lq.end[i], sweptIdx: lq.swept[i] || null,
      });
    }
    return {
      ok: true, swingLength: swingLen, n,
      swings, bos: bosList, fvg: fvgList, ob: obList, liquidity: liqList,
      retrace: { dir: rt.dir, cur: rt.cur, deep: rt.deep },
      prevHigh: phl.prevHigh, prevLow: phl.prevLow,
      brokenHigh: phl.brokenHigh, brokenLow: phl.brokenLow,
      _raw: { sw, bc, fv, ob, lq, rt, phl },
    };
  }

  TV.pa2 = {
    compute, DEFAULT_SWING, SWING_OPTIONS, SWING_DEFAULTS,
    swingHighsLows, bosChoch, fairValueGaps, orderBlocks, liquidity, retracements, previousHighLow,
    dropNarrowFvg, MIN_GAP_ATR,   // 仅供对拍校验，生产路径走 setData
    last: null,
  };

  /* ================================================================
     图层管理器（DOM 相关；导入本文件不会触发任何 DOM 访问）
     ================================================================ */
  const PREF_ON = 'tv.pa2.on';
  const PREF_SWING = 'tv.pa2.swing';
  let enabled = true;
  let swingLength = SWING_DEFAULTS.day;
  let swingSource = 'default';      // default / custom / fallback（根数不足 3L 时自动降档）
  const swingOverrides = {};        // 每档独立记忆：{ day:20, '5':50, ... }
  let legacySwing = 0;              // 旧版全局标量，首次 setData 时迁到日线
  let prim = null;
  let pop = null;   // 挂在 SMC 按钮齿轮下的浮层（与 VP /「形」按钮浮层同款）
  let lastBars = null;
  let lastPeriod = 'day';

  try { enabled = localStorage.getItem(PREF_ON) !== '0'; } catch (_) {}
  try {
    const raw = localStorage.getItem(PREF_SWING);
    if (raw) {
      if (raw.charAt(0) === '{') {
        const o = JSON.parse(raw);
        for (const k of Object.keys(SWING_DEFAULTS)) {
          const v = +o[k];
          if (SWING_OPTIONS.includes(v)) swingOverrides[k] = v;
        }
      } else {
        const v = +raw;
        if (SWING_OPTIONS.includes(v)) legacySwing = v;
      }
    }
  } catch (_) {}

  // swingHighsLows() 的检测区间是 [2L-1, n-1-L]，n >= 3L 才会出现第一个候选点。
  // compute() 的 2L+5 是更低的「能算」门槛，可能算出 0 摆动；选项可用性按 3L 判。
  function minBarsForSwing(L) { return 3 * L; }

  function maxSwingFor(n) {
    let best = 0;
    for (const o of SWING_OPTIONS) if (minBarsForSwing(o) <= n) best = o;
    return best;
  }

  // 手动值 → 本档默认 → 根数允许的最大值；保存值在短历史上临时失效但不清除
  function resolveSwing(period, n) {
    const def = SWING_DEFAULTS[period] || DEFAULT_SWING;
    const valid = v => SWING_OPTIONS.includes(v) && (!n || minBarsForSwing(v) <= n);
    const saved = swingOverrides[period];
    if (valid(saved)) return { length: saved, source: 'custom' };
    if (valid(def)) return { length: def, source: 'default' };
    const max = n ? maxSwingFor(n) : 0;
    return { length: max || SWING_OPTIONS[0], source: 'fallback' };
  }

  function saveSwingPrefs() {
    try { localStorage.setItem(PREF_SWING, JSON.stringify(swingOverrides)); } catch (_) {}
  }

  function ensurePrim() {
    if (prim) return prim;
    if (!TV.chart || !TV.chart.candle) return null;
    prim = new TV.Pa2Primitive();
    TV.chart.candle.attachPrimitive(prim);
    return prim;
  }

  /* A 股日线跳空多、噪声缺口大：gap 宽度 < 0.15×ATR60 的 FVG 不上图不上报。
     实测 30 只 × 1200 根共 3860 个 FVG，其中 21.0% 落在该阈值下（含一字板的仅 1.1%，
     零宽缺口 0%——FVG 定义要求严格 gap）。这是 tv 自有口径，偏离上游 0.0.27 与 zg，
     故只作用于消费层：compute()._raw 保持全量，与 Python 的逐点对拍证据不受影响。 */
  function dropNarrowFvg(r, bars) {
    const n = bars.length;
    const pre = new Float64Array(n + 1);           // TR 前缀和：pre[k] = Σ TR[0..k-1]
    for (let j = 0; j < n; j++) {
      const b = bars[j], p = bars[j - 1];
      const h = +b.high, l = +b.low;
      pre[j + 1] = pre[j] + (p == null ? h - l
        : Math.max(h - l, Math.abs(h - +p.close), Math.abs(l - +p.close)));
    }
    const atrAt = i => {
      const lo = Math.max(1, i - ATR_LEN + 1), m = i - lo + 1;
      return m < ATR_MIN_SAMPLES ? null : (pre[i + 1] - pre[lo]) / m;
    };
    const kept = [], dropped = [];
    for (const z of r.fvg) {
      const a = atrAt(z.idx);
      if (a != null && a > 0 && Math.abs(z.top - z.bottom) < MIN_GAP_ATR * a) dropped.push(z);
      else kept.push(z);
    }
    r.fvg = kept;
    r.fvgDropped = dropped;
    r.minGapAtr = MIN_GAP_ATR;
  }

  // 构造渲染载荷：只画「未回补」的 FVG/OB 与近 120 根内已回补的（口径同 zg）
  function buildPayload(r, bars) {    const t = bars.length - 1;
    const swings = r.swings.map(s => ({ i: s.idx, kind: s.kind, level: s.level }));
    const bos = r.bos.map(s => ({
      i: s.idx, j: s.brokenIdx != null ? s.brokenIdx : s.idx,
      level: s.level, kind: s.kind, dir: s.dir,
    }));
    const fvg = [];
    for (const z of r.fvg) {
      const open = z.mitigatedIdx == null;
      if (!open && z.idx < t - 120) continue;
      fvg.push({
        i: z.idx, endI: open ? t : z.mitigatedIdx,
        top: z.top, bottom: z.bottom, dir: z.dir, open,
      });
    }
    const ob = [];
    for (const z of r.ob) {
      const open = z.mitigatedIdx == null;
      if (!open && z.idx < t - 120) continue;
      ob.push({
        i: z.idx, endI: open ? t : z.mitigatedIdx,
        top: z.top, bottom: z.bottom, dir: z.dir, open,
      });
    }
    const liq = r.liquidity.map(z => ({
      i: z.idx, endI: z.sweptIdx != null ? z.sweptIdx : t, level: z.level,
    }));
    return { bars, swings, bos, fvg, ob, liq, obLabel: 'OB', phl: r };
  }

  function setData(bars, period) {
    lastBars = bars || null;
    lastPeriod = period || 'day';
    // 旧版只有一个全局标量，无法知道当年设置在哪个周期：统一迁到日线（老 UI 的主档），
    // 不绑首个加载档——否则先打开 26 根年线会把值存成永远不可用的自定义。
    // 旧值等于日线默认 10 时不落自定义，界面直接显示「本档默认」。
    if (legacySwing) {
      if (!swingOverrides.day && legacySwing !== SWING_DEFAULTS.day) {
        swingOverrides.day = legacySwing;
      }
      legacySwing = 0;
      saveSwingPrefs();
    }
    const pick = resolveSwing(lastPeriod, bars ? bars.length : 0);
    swingLength = pick.length;
    swingSource = pick.source;
    if (!bars || !bars.length) {
      TV.pa2.last = null;
      const p = ensurePrim();
      if (p) p.setData(null);
      syncPop();
      return;
    }
    // 关闭时不做全量 SMC 计算（1 分钟档 2.88 万根主线程卡数百 ms），开启时用 lastBars 补算
    if (!enabled) {
      TV.pa2.last = null;
      const p = ensurePrim();
      if (p) p.setData(null);
      syncPop();
      return;
    }
    const r = compute(bars, { swingLength, timeFrame: 'W' });
    if (r.ok) dropNarrowFvg(r, bars);
    TV.pa2.last = r;
    const p = ensurePrim();
    if (p) {
      p.setData(r.ok ? buildPayload(r, bars) : null);
      p.setVisible(enabled && r.ok);
    }
    syncPop();
  }

  // ---------------------------------------------------------------- 浮层
  // 与 VP /「形」按钮浮层同款（.ind-params）：点 SMC 右侧齿轮打开，
  // 装摆动长度 + 统计 + BOS/CHoCH 表（原侧栏 renderPa2 的内容 1:1 搬入）。

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g,
      c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  function popBody() {
    const d = debug();
    if (!d.enabled) return '<div class="dim">PA2 图层已关闭（顶栏「SMC」开启）</div>';
    if (!d.ok) return `<div class="dim">${esc(d.reason || '无法计算')}</div>`;
    const r = TV.pa2.last;
    const n = r.n;
    const fvgOpen = r.fvg.filter(x => x.mitigatedIdx == null).length;
    const obOpen = r.ob.filter(x => x.mitigatedIdx == null).length;
    const rtDir = r.retrace.dir[n - 1];
    const rtCur = r.retrace.cur[n - 1];
    const rtTxt = (rtDir === 0 || !Number.isFinite(rtCur))
      ? '—' : `${rtCur.toFixed(1)}%（自${rtDir > 0 ? '高' : '低'}点）`;
    const srcTxt = d.swingSource === 'custom' ? '本档自定义'
      : d.swingSource === 'default' ? `本档默认 ${d.swingDefault}` : '自动降档';
    const rows = r.bos.slice(-30).reverse().map(s => `
        <tr title="${esc(s.kind === 'bos' ? 'BOS 结构突破' : 'CHoCH 结构改变')}">
          <td>${s.kind === 'bos' ? 'BOS' : 'CHoCH'}</td>
          <td class="${s.dir > 0 ? 'up' : 'down'}">${s.dir > 0 ? '↑' : '↓'}</td>
          <td class="r">${s.level.toFixed(2)}</td>
          <td class="r">${esc(String(s.time || '').slice(5, 10))}</td>
        </tr>`).join('');
    return `
      <div class="dim" style="margin-bottom:4px">摆动 ${d.swings} · BOS ${d.bos} / CHoCH ${d.choch}` +
      ` · FVG ${d.fvg}（未回补 ${fvgOpen}）· OB ${d.ob}（有效 ${obOpen}）` +
      ` · 流动性 ${d.liq}</div>` +
      `<div class="dim" style="margin-bottom:4px">回撤 ${rtTxt} · 摆动长度 ${d.swingLength}（${srcTxt}）` +
      ` · 算法移植自 smart-money-concepts 0.0.27</div>` +
      `<div class="dim" style="margin-bottom:4px" title="A 股日线跳空噪声，窄缺口不计入结构；该阈值是 tv 自有口径，上游与 zg 无此过滤">` +
      `窄缺口 FVG 已滤 ${d.fvgDropped} 个（gap &lt; ${d.minGapAtr}×ATR60）</div>` +
      `<div class="candle-list">
        <table class="grid grid4 pa2-grid"><thead><tr><th>类型</th><th>方向</th><th>价位</th><th class="r">时间</th></tr></thead><tbody>` +
      rows +
      '</tbody></table></div>';
  }

  function popHtml() {
    const n = lastBars ? lastBars.length : 0;
    const def = SWING_DEFAULTS[lastPeriod] || DEFAULT_SWING;
    return `
      <div class="ind-params-title">PA2 结构</div>
      <label class="ind-params-row">
        <span>摆动长度</span>
        <select id="pa2-swing" class="mini" title="摆动长度：按周期档默认（1/5 分钟 50、15/30 分钟 20、60 分钟/日/周 10、月/季/年 5），每档独立记忆">
          ${SWING_OPTIONS.map(o => {
            const tooBig = n > 0 && minBarsForSwing(o) > n;
            return `<option value="${o}"${o === swingLength ? ' selected' : ''}${tooBig ? ' disabled' : ''}>${o}${o === def ? '（默认）' : ''}</option>`;
          }).join('')}
        </select>
      </label>` + popBody();
  }

  function fillPop(el) {
    const sel = el.querySelector('#pa2-swing');
    if (sel) sel.onchange = () => setSwing(sel.value);
  }

  function renderPop() {
    if (!pop) return;
    pop.el.innerHTML = popHtml();
    fillPop(pop.el);
  }

  /* setData/setVisible 后浮层若开着，即时重渲染（统计与表随最新 bars/开关变化） */
  function syncPop() {
    if (pop) renderPop();
  }

  function closePop() {
    TV.ui.close(); // onClosed 里置 pop = null
  }

  function openPop(anchor) {
    TV.ui.open({
      id: 'pa2-pop', cls: 'pa2-pop', anchor, anchorSel: '#pa2-toggle',
      render: el => { pop = { el }; renderPop(); },
      onClosed: () => { pop = null; },
    });
  }

  function setVisible(on) {
    enabled = !!on;
    try { localStorage.setItem(PREF_ON, enabled ? '1' : '0'); } catch (_) {}
    const btn = document.getElementById('pa2-toggle');
    if (btn) btn.classList.toggle('on', enabled);
    if (enabled && lastBars && lastBars.length) { setData(lastBars, lastPeriod); return; } // 开启时补算
    const p = ensurePrim();
    if (p) p.setVisible(enabled && !!(TV.pa2.last && TV.pa2.last.ok));
    syncPop();
  }

  function setSwing(n) {
    const v = +n;
    if (!SWING_OPTIONS.includes(v)) return;
    if (lastBars && lastBars.length && minBarsForSwing(v) > lastBars.length) return;   // 本档根数不够，拒绝且不落盘
    const def = SWING_DEFAULTS[lastPeriod] || DEFAULT_SWING;
    if (v === def) delete swingOverrides[lastPeriod];      // 选回本档默认 = 清除自定义
    else swingOverrides[lastPeriod] = v;
    legacySwing = 0;
    saveSwingPrefs();
    if (lastBars) setData(lastBars, lastPeriod);           // setData 会从 overrides 取回 v
    else { swingLength = v; swingSource = v === def ? 'default' : 'custom'; syncPop(); }
  }

  function init() {
    const btn = document.getElementById('pa2-toggle');
    if (btn) {
      btn.onclick = () => setVisible(!enabled);
      btn.classList.toggle('on', enabled);
      // 齿轮：点它开 PA2 结构浮层（不触发开关）
      const gear = document.createElement('i');
      gear.className = 'ind-gear';
      gear.id = 'pa2-gear';
      gear.title = 'PA2 结构统计与 BOS/CHoCH 列表';
      gear.textContent = '⚙';
      gear.addEventListener('click', e => {
        e.preventDefault();
        e.stopPropagation();
        if (pop) closePop(); else openPop(gear);
      });
      btn.appendChild(gear);
    }
    ensurePrim();
  }

  function debug() {
    const r = TV.pa2.last;
    return {
      enabled, period: lastPeriod, swingLength,
      swingDefault: SWING_DEFAULTS[lastPeriod] || DEFAULT_SWING,
      swingSource,
      swingSaved: swingOverrides[lastPeriod] || null,
      pop: !!pop,
      ok: !!(r && r.ok),
      reason: r && !r.ok ? r.reason : null,
      swings: r && r.ok ? r.swings.length : 0,
      bos: r && r.ok ? r.bos.filter(x => x.kind === 'bos').length : 0,
      choch: r && r.ok ? r.bos.filter(x => x.kind === 'choch').length : 0,
      fvg: r && r.ok ? r.fvg.length : 0,
      fvgDropped: r && r.ok ? (r.fvgDropped || []).length : 0,
      minGapAtr: MIN_GAP_ATR,
      ob: r && r.ok ? r.ob.length : 0,
      liq: r && r.ok ? r.liquidity.length : 0,
    };
  }

  TV.pa2Layer = { init, setData, setVisible, setSwing, toggle: () => setVisible(!enabled), debug,
    openPop, closePop };
})();
