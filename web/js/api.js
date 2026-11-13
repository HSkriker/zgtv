(function () {
  const TV = (window.TV = window.TV || {});

  // 取数拆成「带字节数的版本」：字节数只用于 LRU 淘汰决策，不必缓存就不用它。
  async function fetchJSON(url) {
    const r = await fetch(url);
    const text = await r.text();
    let j;
    try { j = JSON.parse(text); } catch (_) { throw new Error(`非 JSON 响应（HTTP ${r.status}）`); }
    if (!r.ok || j.ok === false) throw new Error(j.error || `HTTP ${r.status}`);
    // 字节数按真实 UTF-8 计（Blob.size）：行情 JSON 以数字为主但含中文名，
    // text.length × 2 会把 26MB 的正文估成 52MB，导致满深度分钟档永远进不了缓存。
    // Blob 构造是原生 O(n)，比 JSON.stringify 再量一遍便宜；只用于淘汰决策，不用于精确计费。
    const bytes = typeof Blob !== 'undefined' ? new Blob([text]).size : text.length * 2;
    return { j, bytes };
  }

  async function getJSON(url) {
    return (await fetchJSON(url)).j;
  }

  async function postJSON(url, body) {
    const r = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    const text = await r.text();
    let j;
    try { j = JSON.parse(text); } catch (_) { throw new Error(`非 JSON 响应（HTTP ${r.status}）`); }
    if (!r.ok || j.ok === false) throw new Error(j.error || `HTTP ${r.status}`);
    return j;
  }

  // 满深度行情（120 交易日的 1 分钟 ≈ 2.9 万根，实测 22~52MB）在浏览器侧同样不能无限堆：
  // 条数 + 字节双上限，按最近使用淘汰（与后端 cache.rs 同口径）。
  // 64MB 够放一条最大的满深度分钟线（000001 实测 51.5MB），又不会让标签页吃到几百 MB。
  const MEM_MAX = 48;
  const MEM_BYTES_MAX = 64 * 1024 * 1024;
  const MEM = new Map();     // key -> { j, bytes }
  let memBytes = 0;

  function memDrop(key) {
    const e = MEM.get(key);
    if (!e) return;
    MEM.delete(key);
    memBytes -= e.bytes;
  }

  function memPut(key, e) {
    if (MEM.has(key)) memDrop(key);
    // 单条就超过总预算：不入缓存，也不清空已有条目。
    // 旧行为是「先塞进去再从头淘汰到空」——一次满深度分钟线会把 symbols / 行业 / 总览
    // 全部挤掉，切回时整页重拉；而这些小条目加起来通常只有几百 KB。
    if (e.bytes > MEM_BYTES_MAX) return;
    MEM.set(key, e);
    memBytes += e.bytes;
    // 新条目自己没超限，但累加后可能超：按 LRU 继续淘汰到上限内
    while (MEM.size > MEM_MAX || (memBytes > MEM_BYTES_MAX && MEM.size > 0)) {
      memDrop(MEM.keys().next().value);
    }
  }

  async function cached(key, url) {
    if (MEM.has(key)) {
      const e = MEM.get(key);
      MEM.delete(key);
      MEM.set(key, e);       // LRU touch：只换顺序，不动字节数
      return e.j;
    }
    const e = await fetchJSON(url);
    memPut(key, e);
    return e.j;
  }

  // 标签缓存：静态归属数据，每条几十字节，256 个符号足够一整天浏览
  const TAGS_MAX = 256;
  const TAGS = new Map();

  TV.api = {
    getJSON,
    symbols() {
      return cached('symbols', '/api/symbols');
    },
    bars(code, period, lmt = 1200, fresh = false, pset = '') {
      const key = `bars|${code}|${period}|${lmt}|${pset}`;
      if (fresh) memDrop(key);
      const url = `/api/bars?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}&lmt=${lmt}`
        + (pset ? `&pset=${encodeURIComponent(pset)}` : '');
      return cached(key, url);
    },
    // 个股 L2 行业 + 概念题材（静态归属数据）：独立小缓存，不与行情 LRU 抢位置
    stockTags(code) {
      const key = `tags|${code}`;
      if (TAGS.has(key)) return Promise.resolve(TAGS.get(key));
      return getJSON(`/api/stock-tags?code=${encodeURIComponent(code)}`).then((j) => {
        TAGS.set(key, j);
        while (TAGS.size > TAGS_MAX) TAGS.delete(TAGS.keys().next().value);
        return j;
      });
    },
    boardBars(code, period, lmt = 1200, fresh = false, pset = '') {
      const key = `board|${code}|${period}|${lmt}|${pset}`;
      if (fresh) memDrop(key);
      const url = `/api/board-kline?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}&lmt=${lmt}`
        + (pset ? `&pset=${encodeURIComponent(pset)}` : '');
      return cached(key, url);
    },
    chan(code, period, fresh = false, board = false) {
      const key = `chan|${board ? 'b' : 's'}|${code}|${period}`;
      if (fresh) memDrop(key);
      const url = `/api/chan?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}`
        + (board ? '&board=1' : '');
      return cached(key, url);
    },
    scan(code, freq = 'D', limit = 50) {
      return getJSON(`/api/scan?code=${encodeURIComponent(code)}&freq=${freq}&limit=${limit}`);
    },
    ind(code, period, lmt, name, board = false, fresh = false, params = '') {
      const p = params ? `|${params}` : '';
      const key = `ind|${board ? 'b' : 's'}|${code}|${period}|${lmt}|${name}${p}`;
      if (fresh) memDrop(key);
      const url = `/api/ind?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}`
        + `&lmt=${lmt}&name=${encodeURIComponent(name)}`
        + (params ? `&p=${encodeURIComponent(params)}` : '')
        + (board ? '&board=1' : '');
      return cached(key, url);
    },
    heat(fresh = false) {
      const key = 'heat';
      if (fresh) memDrop(key);
      return cached(key, '/api/heat');
    },
    marketIndices(fresh = false) {
      const key = 'market-indices';
      if (fresh) memDrop(key);
      return cached(key, '/api/market-indices');
    },
    sectorOverview(fresh = false) {
      const key = 'sector-overview';
      if (fresh) memDrop(key);
      const url = '/api/sector-overview' + (fresh ? '?force=1' : '');
      return cached(key, url);
    },
    indexConstituents(code, fresh = false) {
      const key = `idxconst|${code}`;
      if (fresh) memDrop(key);
      return cached(key, `/api/index-constituents?code=${encodeURIComponent(code)}`);
    },
    quote(code) {
      return getJSON(`/api/quote?code=${encodeURIComponent(code)}`);
    },
    live(codes) {
      return getJSON(`/api/live?codes=${encodeURIComponent(codes.join(','))}`);
    },
    marketStatus() {
      return getJSON('/api/market-status');
    },
    drawings(code, period) {
      return getJSON(`/api/drawings?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}`);
    },
    saveDrawings(code, period, rows) {
      return postJSON(`/api/drawings?code=${encodeURIComponent(code)}&period=${encodeURIComponent(period)}`, { rows });
    },
    // 只读验收口：确认 LRU 的条数/字节双上限真的生效（不在业务代码里调用）。
    _cacheStats() {
      return {
        entries: MEM.size,
        bytes: memBytes,
        maxEntries: MEM_MAX,
        maxBytes: MEM_BYTES_MAX,
      };
    },
  };
})();
