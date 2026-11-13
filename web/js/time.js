(function () {
  const TV = (window.TV = window.TV || {});

  // 成交量统一格式化（主图图例与 VP 浮层共用；NaN/undefined 显式给 '-'，不再出现「NaN手」）
  function fmtVol(v) {
    if (!Number.isFinite(Number(v))) return '-';
    v = Number(v);
    if (v >= 1e8) return (v / 1e8).toFixed(2) + '亿手';
    if (v >= 1e4) return (v / 1e4).toFixed(2) + '万手';
    return Math.round(v) + '手';
  }

  /* 每周期一次拉满的根数（0 = 本地有多少给多少）。
     日线按 10 年 ≈ 2430 个交易日；周/月/季/年直接全量（600519 分别 1270/302/102/26 根）。
     分钟按 120 个交易日折算：1 分 240 根/日、5 分 48、15 分 16、30 分 8、60 分 4。
     本地 .lc1/.lc5 是通达信滚动窗口，实测固定 168 个交易日，120 日在范围内。 */
  const MIN_DAYS = 120;
  const PER_DAY = { '1': 240, '5': 48, '15': 16, '30': 8, '60': 4 };
  const DEPTH = {
    day: 2430, week: 0, month: 0, quarter: 0, year: 0,
    '1': MIN_DAYS * PER_DAY['1'],
    '5': MIN_DAYS * PER_DAY['5'],
    '15': MIN_DAYS * PER_DAY['15'],
    '30': MIN_DAYS * PER_DAY['30'],
    '60': MIN_DAYS * PER_DAY['60'],
  };

  const CN = {
    day: '日线', week: '周线', month: '月线', quarter: '季线', year: '年线',
    '1': '1分钟', '5': '5分钟', '15': '15分钟', '30': '30分钟', '60': '60分钟',
  };

  // 与后端 zg_chan::service::MIN_BARS 同口径：低于这个根数缠论直接拒算
  const MIN_CHAN_BARS = 60;

  TV.time = {
    MIN_DAYS, PER_DAY, DEPTH, CN, MIN_CHAN_BARS,
    fmtVol,
    // 分钟周期：1 / 5 / 15 / 30 / 60
    isIntraday(period) {
      return /^\d+$/.test(String(period));
    },
    lmt(period) {
      const v = DEPTH[String(period)];
      return v == null ? 1200 : v;
    },
    label(period) {
      return CN[String(period)] || String(period || '');
    },
    // 年线最多 29 根，过不了缠论的 60 根下限；这一档只有 K 线
    hasChan(period) {
      return String(period) !== 'year';
    },
  };
})();
