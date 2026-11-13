(function () {
  const TV = (window.TV = window.TV || {});

  // 移植自 TradingView plugin-examples/bands-indicator 的填色思路：
  // 上下轨之间铺一层半透明多边形。带宽本身仍由两条 LineSeries 画（它们自带
  // autoscale，所以这里不需要 bands-indicator 的 autoscaleInfo 兜底）。
  const MARGIN = 400; // 视口两侧多留一段，避免缩放时边缘出现没填到的细缝

  class BandRenderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const o = this._owner;
      const p = o.param;
      if (!p) return;
      const ts = p.chart.timeScale();
      const ser = p.series;
      const rows = o.rows;
      if (!rows.length) return;

      target.useMediaCoordinateSpace(scope => {
        const w = scope.mediaSize.width;
        const ctx = scope.context;
        const top = [];
        const bot = [];
        for (let i = 0; i < rows.length; i++) {
          const r = rows[i];
          const x = ts.timeToCoordinate(r.t);
          if (x == null || x < -MARGIN || x > w + MARGIN) continue;
          const yu = ser.priceToCoordinate(r.up);
          const yl = ser.priceToCoordinate(r.low);
          if (yu == null || yl == null) continue;
          top.push(x, yu);
          bot.push(x, yl);
        }
        if (top.length < 6) return;

        ctx.beginPath();
        ctx.moveTo(top[0], top[1]);
        for (let i = 2; i < top.length; i += 2) ctx.lineTo(top[i], top[i + 1]);
        for (let i = bot.length - 2; i >= 0; i -= 2) ctx.lineTo(bot[i], bot[i + 1]);
        ctx.closePath();
        ctx.fillStyle = o.fill;
        ctx.fill();
      });
    }
  }

  class BandPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new BandRenderer(owner);
    }
    zOrder() { return 'normal'; }
    renderer() { return this._owner.rows.length ? this._renderer : null; }
  }

  class BandPrimitive {
    constructor() {
      this.rows = [];
      this.fill = 'rgba(45, 212, 191, 0.13)';   // 与 BOLL 上下轨同色系（teal）
      this.param = null;
      this._view = new BandPaneView(this);
    }
    attached(param) { this.param = param; }
    detached() { this.param = null; }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    setData(rows) {
      this.rows = rows || [];
      if (this.param) this.param.requestUpdate();
    }
  }

  TV.BandPrimitive = BandPrimitive;
})();
