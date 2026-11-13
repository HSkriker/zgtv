// PA2 · SMC 图层渲染（lightweight-charts primitive，禁 ECharts）
// 视觉口径对齐 zg pa2-layer.js：摆动点 / BOS·CHoCH 水平线 / FVG·OB 半透明区块 / 流动性池。
// 有意分歧（2026-11-01 用户决定）：半透明区块填色按 A 股红涨绿跌习惯对调——
// 看涨=红粉（OB 红 / FVG 粉）、看跌=绿蓝（OB 绿 / FVG 蓝）；zg pa2-layer.js 是绿涨/红跌。
(function () {
  const TV = (window.TV = window.TV || {});

  const COL = {
    // 摆动点同区块口径：前低=支撑参考用暖色（粉），前高=压力参考用冷色（蓝）
    swHigh: '#38bdf8', swLow: '#f472b6',
    bos: '#f59e0b', choch: '#a855f7',
    liq: '#22d3ee',
    fvgUp: 'rgba(244,114,182,', fvgDn: 'rgba(56,189,248,',
    obUp: 'rgba(248,113,113,', obDn: 'rgba(74,222,128,',
  };

  function dash(ctx, x1, y1, x2, y2, on, off) {
    ctx.setLineDash(on ? [on, off] : []);
    ctx.moveTo(x1, y1);
    ctx.lineTo(x2, y2);
    ctx.setLineDash([]);
  }

  class Pa2Renderer {
    constructor(owner) { this._owner = owner; }

    draw(target) {
      const owner = this._owner;
      const d = owner.data;
      if (!d) return;
      const ts = owner.param.chart.timeScale();
      const series = owner.param.series;
      const bars = d.bars;

      const xOf = i => {
        if (i == null || i < 0 || i >= bars.length) return null;
        return ts.timeToCoordinate(bars[i].time);
      };
      const yOf = p => series.priceToCoordinate(p);

      target.useMediaCoordinateSpace(scope => {
        const ctx = scope.context;
        ctx.save();
        ctx.lineCap = 'butt';

        // ① FVG / OB 区块（先画，压在底层）
        const area = (rows, colorUp, colorDown, alpha, labelText) => {
          for (const z of rows) {
            const x1 = xOf(z.i);
            const x2 = xOf(z.endI);
            if (x1 == null || x2 == null) continue;
            const yHi = yOf(Math.max(z.top, z.bottom));
            const yLo = yOf(Math.min(z.top, z.bottom));
            if (yHi == null || yLo == null) continue;
            const y = Math.min(yHi, yLo);
            const h = Math.abs(yLo - yHi);
            if (h < 1) continue;
            const x = Math.min(x1, x2);
            const w = Math.abs(x2 - x1);
            ctx.fillStyle = (z.dir > 0 ? colorUp : colorDown) + alpha + ')';
            ctx.fillRect(x, y, w, h);
            if (z.open && w >= 34 && h >= 12) {
              ctx.font = '9px sans-serif';
              ctx.textBaseline = 'top';
              // 区块标签色随填色方向走：看涨=浅粉、看跌=浅蓝
              ctx.fillStyle = z.dir > 0 ? '#f9a8d4' : '#7dd3fc';
              ctx.fillText(labelText, x + 3, y + 2);
            }
          }
        };
        area(d.fvg, COL.fvgUp, COL.fvgDn, 0.18, 'FVG');
        area(d.ob, COL.obUp, COL.obDn, 0.16, d.obLabel);

        // ② 流动性池：水平虚线 + 端点标签
        ctx.lineWidth = 1;
        ctx.strokeStyle = COL.liq;
        for (const z of d.liq) {
          const x1 = xOf(z.i);
          const x2 = xOf(z.endI);
          const y = yOf(z.level);
          if (x1 == null || x2 == null || y == null) continue;
          dash(ctx, x1, y, x2, y, 3, 3);
          ctx.stroke();
          ctx.fillStyle = COL.liq;
          ctx.beginPath();
          ctx.arc(x1, y, 2, 0, Math.PI * 2);
          ctx.fill();
          if (x2 - x1 >= 18) {
            ctx.font = '9px sans-serif';
            ctx.textBaseline = 'bottom';
            ctx.fillText('LQ', x2, y - 2);
          }
        }

        // ③ BOS / CHoCH：结构位 → 破坏 K 线的水平线 + 端点标签
        ctx.lineWidth = 1.2;
        for (const z of d.bos) {
          const x1 = xOf(z.i);
          const x2 = xOf(z.j);
          const y = yOf(z.level);
          if (x1 == null || x2 == null || y == null) continue;
          ctx.strokeStyle = z.kind === 'bos' ? COL.bos : COL.choch;
          dash(ctx, x1, y, x2, y, 5, 4);
          ctx.stroke();
          ctx.fillStyle = ctx.strokeStyle;
          ctx.beginPath();
          ctx.arc(x2, y, 2.2, 0, Math.PI * 2);
          ctx.fill();
          const txt = (z.kind === 'bos' ? 'BOS' : 'CHoCH') + (z.dir > 0 ? '↑' : '↓');
          ctx.font = '9px sans-serif';
          ctx.textBaseline = z.dir > 0 ? 'bottom' : 'top';
          ctx.fillText(txt, x2, y + (z.dir > 0 ? -3 : 3));
        }

        // ④ 摆动点
        for (const z of d.swings) {
          const x = xOf(z.i);
          const y = yOf(z.level);
          if (x == null || y == null) continue;
          ctx.fillStyle = z.kind === 1 ? COL.swHigh : COL.swLow;
          ctx.beginPath();
          ctx.arc(x, y, 2.6, 0, Math.PI * 2);
          ctx.fill();
        }
        ctx.restore();
      });
    }
  }

  class Pa2PaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new Pa2Renderer(owner);
    }
    zOrder() { return 'bottom'; }
    renderer() {
      const o = this._owner;
      return o.visible && o.data && o.data.bars && o.data.bars.length ? this._renderer : null;
    }
  }

  class Pa2Primitive {
    constructor() {
      this.data = null;
      this.visible = true;
      this.param = null;
      this._view = new Pa2PaneView(this);
    }
    attached(param) { this.param = param; }
    detached() { this.param = null; }
    updateAllViews() {}
    paneViews() { return [this._view]; }
    setData(d) {
      this.data = d || null;
      if (this.param) this.param.requestUpdate();
    }
    setVisible(on) {
      this.visible = !!on;
      if (this.param) this.param.requestUpdate();
    }
  }

  TV.Pa2Primitive = Pa2Primitive;
})();
