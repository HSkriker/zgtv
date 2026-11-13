(function () {
  const TV = (window.TV = window.TV || {});

  function stateColor(s) {
    if (s === '新生') return '#f0b90b';
    if (s === '扩张') return '#ff9800';
    return 'rgba(150, 165, 195, 0.95)';
  }

  class ZsRenderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const owner = this._owner;
      const p = owner.param;
      if (!p) return;
      const ts = p.chart.timeScale();
      const rows = owner.data;

      target.useMediaCoordinateSpace(scope => {
        const ctx = scope.context;
        for (const z of rows) {
          const x1 = ts.timeToCoordinate(z.sdt);
          const x2 = ts.timeToCoordinate(z.edt);
          const y1 = p.series.priceToCoordinate(z.zg);
          const y2 = p.series.priceToCoordinate(z.zd);
          if (x1 == null || x2 == null || y1 == null || y2 == null) continue;

          const x = Math.min(x1, x2);
          const y = Math.min(y1, y2);
          const w = Math.abs(x2 - x1);
          const h = Math.abs(y2 - y1);
          if (w < 1 || h < 1) continue;

          ctx.fillStyle = 'rgba(41, 98, 255, 0.08)';
          ctx.fillRect(x, y, w, h);
          ctx.strokeStyle = 'rgba(91, 141, 239, 0.55)';
          ctx.lineWidth = 1;
          ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);

          if (z.state && h >= 10) {
            ctx.font = '11px sans-serif';
            ctx.textBaseline = 'top';
            ctx.fillStyle = stateColor(z.state);
            if (w >= 28) ctx.fillText(z.state, x + 4, y + 3);
            else ctx.fillText(z.state, x, Math.max(12, y - 14));
          }
        }
      });
    }
  }

  class ZsPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new ZsRenderer(owner);
    }
    zOrder() { return 'normal'; }
    renderer() {
      return this._owner.visible && this._owner.data.length ? this._renderer : null;
    }
  }

  class ZsPrimitive {
    constructor() {
      this.data = [];
      this.visible = true;
      this.param = null;
      this._view = new ZsPaneView(this);
    }
    attached(param) { this.param = param; }
    detached() { this.param = null; }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    setData(rows) {
      this.data = rows || [];
      if (this.param) this.param.requestUpdate();
    }
    setVisible(on) {
      this.visible = !!on;
      if (this.param) this.param.requestUpdate();
    }
  }

  TV.ZsPrimitive = ZsPrimitive;
})();
