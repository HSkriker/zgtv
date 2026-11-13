(function () {
  const TV = (window.TV = window.TV || {});

  function lineCoords(p, s, ts) {
    const x1 = ts.timeToCoordinate(s.t1);
    const y1 = p.series.priceToCoordinate(s.p1);
    const x2 = ts.timeToCoordinate(s.t2);
    const y2 = p.series.priceToCoordinate(s.p2);
    return (x1 == null || y1 == null || x2 == null || y2 == null) ? null : { x1, y1, x2, y2 };
  }

  const FIB_LEVELS = [0, 0.236, 0.382, 0.5, 0.618, 0.786, 1];

  function timeMs(t) {
    if (typeof t === 'number') return t * 1000;
    if (t && typeof t === 'object' && t.year) return Date.UTC(t.year, t.month - 1, t.day);
    if (typeof t === 'string') {
      const d = Date.parse(t.length === 10 ? t + 'T00:00:00Z' : t);
      return Number.isFinite(d) ? d : null;
    }
    return null;
  }

  function spanText(t1, t2) {
    const a = timeMs(t1);
    const b = timeMs(t2);
    if (a == null || b == null) return '';
    const sec = Math.abs(b - a) / 1000;
    if (typeof t1 === 'number' || typeof t2 === 'number') {
      if (sec >= 86400) {
        const d = Math.floor(sec / 86400);
        const h = Math.round((sec - d * 86400) / 3600);
        return h ? `${d}天${h}小时` : `${d}天`;
      }
      if (sec >= 3600) {
        const h = Math.floor(sec / 3600);
        const m = Math.round((sec - h * 3600) / 60);
        return m ? `${h}小时${m}分` : `${h}小时`;
      }
      if (sec >= 60) return `${Math.round(sec / 60)}分`;
      return `${Math.round(sec)}秒`;
    }
    return `${Math.round(sec / 86400)}天`;
  }

  function barIndex(t) {
    const st = TV.app && TV.app.state;
    if (!st || !st.timeIndex || !st.timeIndex.get) return -1;
    const i = st.timeIndex.get(t);
    return i == null ? -1 : i;
  }

  class DrawRenderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const owner = this._owner;
      const p = owner.param;
      if (!p) return;
      const ts = p.chart.timeScale();

      target.useMediaCoordinateSpace(scope => {
        const ctx = scope.context;
        const W = scope.mediaSize.width;
        const H = scope.mediaSize.height;
        ctx.lineWidth = 1;

        let fmtPrice = v => Number(v).toFixed(2);
        try { fmtPrice = v => p.series.priceFormatter().format(v); } catch (_) {}

        for (const s of owner.data) {
          if (s.kind !== 'vline') continue;
          const x = ts.timeToCoordinate(s.t);
          if (x == null) continue;
          ctx.save();
          ctx.strokeStyle = 'rgba(240, 185, 11, 0.55)';
          ctx.setLineDash([4, 4]);
          ctx.beginPath();
          ctx.moveTo(x, 0);
          ctx.lineTo(x, H);
          ctx.stroke();
          ctx.restore();
        }

        for (const s of owner.data) {
          if (s.kind !== 'pline') continue;
          const y = p.series.priceToCoordinate(s.p);
          if (y == null) continue;
          const raw = ts.timeToCoordinate(s.t);
          const x = Math.max(0, raw == null ? 0 : raw);
          ctx.save();
          ctx.strokeStyle = '#f0b90b';
          ctx.setLineDash([6, 4]);
          ctx.beginPath();
          ctx.moveTo(x, y);
          ctx.lineTo(W, y);
          ctx.stroke();
          ctx.setLineDash([]);
          const label = Number(s.p).toFixed(2);
          ctx.font = '11px sans-serif';
          const tw = ctx.measureText(label).width;
          ctx.fillStyle = 'rgba(240, 185, 11, 0.9)';
          ctx.fillRect(W - tw - 8, y - 14, tw + 6, 14);
          ctx.fillStyle = '#131722';
          ctx.textBaseline = 'top';
          ctx.fillText(label, W - tw - 5, y - 13);
          ctx.restore();
        }

        for (const s of owner.data) {
          if (s.kind !== 'line') continue;
          const c = lineCoords(p, s, ts);
          if (!c) continue;
          ctx.strokeStyle = '#f0b90b';
          ctx.beginPath();
          ctx.moveTo(c.x1, c.y1);
          ctx.lineTo(c.x2, c.y2);
          ctx.stroke();
        }

        for (const s of owner.data) {
          if (s.kind !== 'rect') continue;
          const c = lineCoords(p, s, ts);
          if (!c) continue;

          const x = Math.min(c.x1, c.x2);
          const y = Math.min(c.y1, c.y2);
          const w = Math.abs(c.x2 - c.x1);
          const h = Math.abs(c.y2 - c.y1);
          if (w < 1 || h < 1) continue;

          ctx.fillStyle = 'rgba(240, 185, 11, 0.10)';
          ctx.fillRect(x, y, w, h);
          ctx.strokeStyle = 'rgba(240, 185, 11, 0.75)';
          ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
        }

        for (const s of owner.data) {
          if (s.kind !== 'fib') continue;
          const c = lineCoords(p, s, ts);
          if (!c) continue;

          const top = Math.min(c.y1, c.y2);
          const bottom = Math.max(c.y1, c.y2);
          ctx.save();
          if (bottom - top >= 1) {
            ctx.fillStyle = 'rgba(41, 98, 255, 0.045)';
            ctx.fillRect(0, top, W, bottom - top);
          }

          ctx.strokeStyle = 'rgba(240, 185, 11, 0.45)';
          ctx.setLineDash([3, 3]);
          ctx.beginPath();
          ctx.moveTo(c.x1, c.y1);
          ctx.lineTo(c.x2, c.y2);
          ctx.stroke();
          ctx.setLineDash([]);

          ctx.font = '10px sans-serif';
          ctx.textBaseline = 'middle';
          for (const r of FIB_LEVELS) {
            const price = s.p2 + (s.p1 - s.p2) * r;
            const y = p.series.priceToCoordinate(price);
            if (y == null) continue;
            const strong = r === 0 || r === 1;
            ctx.strokeStyle = strong ? 'rgba(240, 185, 11, 0.9)' : 'rgba(240, 185, 11, 0.5)';
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(W - 34, y);
            ctx.stroke();

            const text = `${r} ${fmtPrice(price)}`;
            const tw = ctx.measureText(text).width;
            const bx = W - 38 - tw;
            ctx.fillStyle = 'rgba(19, 23, 34, 0.85)';
            ctx.fillRect(bx - 3, y - 7, tw + 6, 14);
            ctx.fillStyle = strong ? '#f0b90b' : 'rgba(240, 185, 11, 0.85)';
            ctx.fillText(text, bx, y);
          }

          ctx.fillStyle = '#f0b90b';
          for (const pt of [[c.x1, c.y1], [c.x2, c.y2]]) {
            ctx.beginPath();
            ctx.arc(pt[0], pt[1], 2.5, 0, Math.PI * 2);
            ctx.fill();
          }
          ctx.restore();
        }

        for (const s of owner.data) {
          if (s.kind !== 'text') continue;
          const x = ts.timeToCoordinate(s.t);
          const y = p.series.priceToCoordinate(s.p);
          if (x == null || y == null) continue;
          const text = String(s.text || '');
          ctx.save();
          ctx.font = '12px "Segoe UI", "Microsoft YaHei", sans-serif';
          const tw = ctx.measureText(text).width;
          const bx = Math.min(x + 6, W - tw - 14);
          const by = Math.max(2, y - 18);
          ctx.fillStyle = 'rgba(19, 23, 34, 0.85)';
          ctx.fillRect(bx - 3, by - 2, tw + 8, 18);
          ctx.strokeStyle = 'rgba(240, 185, 11, 0.5)';
          ctx.strokeRect(bx - 3.5, by - 2.5, tw + 9, 19);
          ctx.fillStyle = '#f0b90b';
          ctx.textBaseline = 'top';
          ctx.fillText(text, bx + 1, by);
          ctx.beginPath();
          ctx.arc(x, y, 2.5, 0, Math.PI * 2);
          ctx.fill();
          ctx.restore();
        }

        const m = owner.measure;
        if (m) {
          const c = lineCoords(p, m, ts);
          if (c) {
            const diff = m.p2 - m.p1;
            const us = TV.app && TV.app.state && TV.app.state.colorMode === 'us';
            const col = diff >= 0
              ? (us ? '#26a69a' : '#ef5350')
              : (us ? '#ef5350' : '#26a69a');

            const x = Math.min(c.x1, c.x2);
            const y = Math.min(c.y1, c.y2);
            const w = Math.abs(c.x2 - c.x1);
            const h = Math.abs(c.y2 - c.y1);
            ctx.save();
            if (w >= 1 && h >= 1) {
              ctx.fillStyle = 'rgba(41, 98, 255, 0.07)';
              ctx.fillRect(x, y, w, h);
            }
            ctx.strokeStyle = 'rgba(41, 98, 255, 0.9)';
            ctx.beginPath();
            ctx.moveTo(c.x1, c.y1);
            ctx.lineTo(c.x2, c.y2);
            ctx.stroke();
            ctx.fillStyle = '#2962ff';
            for (const pt of [[c.x1, c.y1], [c.x2, c.y2]]) {
              ctx.beginPath();
              ctx.arc(pt[0], pt[1], 3, 0, Math.PI * 2);
              ctx.fill();
            }
            ctx.restore();

            const i1 = barIndex(m.t1);
            const i2 = barIndex(m.t2);
            const count = i1 >= 0 && i2 >= 0 ? Math.abs(i2 - i1) + 1 : null;
            const pct = m.p1 ? diff / m.p1 * 100 : 0;
            const lines = [
              `${diff >= 0 ? '+' : '-'}${fmtPrice(Math.abs(diff))} `
                + `(${diff >= 0 ? '+' : ''}${pct.toFixed(2)}%)`,
              count != null ? `${count} 根K线` : '',
              spanText(m.t1, m.t2),
            ].filter(Boolean);

            ctx.save();
            ctx.font = '11px "Segoe UI", "Microsoft YaHei", sans-serif';
            let bw = 0;
            for (const ln of lines) bw = Math.max(bw, ctx.measureText(ln).width);
            const boxW = Math.ceil(bw) + 14;
            const boxH = lines.length * 16 + 8;
            let bx = c.x2 + 10;
            if (bx + boxW > W - 4) bx = c.x2 - boxW - 10;
            bx = Math.max(4, Math.min(bx, W - boxW - 4));
            let by = c.y2 - boxH / 2;
            by = Math.max(4, Math.min(by, H - boxH - 4));

            ctx.fillStyle = 'rgba(19, 23, 34, 0.92)';
            ctx.fillRect(bx, by, boxW, boxH);
            ctx.strokeStyle = 'rgba(41, 98, 255, 0.75)';
            ctx.strokeRect(bx + 0.5, by + 0.5, boxW - 1, boxH - 1);

            ctx.textBaseline = 'middle';
            lines.forEach((ln, i) => {
              ctx.fillStyle = i === 0 ? col : '#d1d4dc';
              ctx.fillText(ln, bx + 7, by + 16 + i * 16);
            });
            ctx.restore();
          }
        }

        if (owner.pending) {
          const x = ts.timeToCoordinate(owner.pending.t);
          const y = p.series.priceToCoordinate(owner.pending.p);
          if (x != null && y != null) {
            ctx.fillStyle = '#f0b90b';
            ctx.beginPath();
            ctx.arc(x, y, 3, 0, Math.PI * 2);
            ctx.fill();
          }
        }
      });
    }
  }

  class DrawPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new DrawRenderer(owner);
    }
    zOrder() { return 'top'; }
    renderer() {
      return (this._owner.data.length || this._owner.pending || this._owner.measure)
        ? this._renderer : null;
    }
  }

  class DrawPrimitive {
    constructor() {
      this.data = [];
      this.pending = null;
      this.measure = null;
      this.param = null;
      this._view = new DrawPaneView(this);
    }
    attached(param) { this.param = param; }
    detached() { this.param = null; }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    setData(rows) {
      this.data = rows || [];
      if (this.param) this.param.requestUpdate();
    }
    setPending(p) {
      this.pending = p || null;
      if (this.param) this.param.requestUpdate();
    }
    setMeasure(m) {
      this.measure = m || null;
      if (this.param) this.param.requestUpdate();
    }
  }

  class HoverRenderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const owner = this._owner;
      const p = owner.param;
      if (!p || owner.time == null) return;
      const ts = p.chart.timeScale();

      target.useMediaCoordinateSpace(scope => {
        const x = ts.timeToCoordinate(owner.time);
        if (x == null) return;
        const half = Math.max(2, (ts.options().barSpacing || 7) / 2);
        scope.context.fillStyle = 'rgba(117, 134, 150, 0.10)';
        scope.context.fillRect(x - half, 0, half * 2, scope.mediaSize.height);
      });
    }
  }

  class HoverPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new HoverRenderer(owner);
    }
    zOrder() { return 'bottom'; }
    renderer() { return this._owner.time == null ? null : this._renderer; }
  }

  class HoverPrimitive {
    constructor() {
      this.time = null;
      this.param = null;
      this._view = new HoverPaneView(this);
    }
    attached(param) { this.param = param; }
    detached() { this.param = null; }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    setTime(t) {
      if (this.time === t) return;
      this.time = t;
      if (this.param) this.param.requestUpdate();
    }
  }

  TV.DrawPrimitive = DrawPrimitive;
  TV.HoverPrimitive = HoverPrimitive;
})();
