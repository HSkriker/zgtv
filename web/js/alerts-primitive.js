(function () {
  const TV = (window.TV = window.TV || {});

  // 移植自 TradingView plugin-examples/user-price-alerts（纯前端，无后端）
  const BUTTON_W = 21;
  const BUTTON_H = 21;
  const ALERT_LABEL_H = 17;
  const CENTRE_LABEL_H = 20;
  const CENTRE_LABEL_PAD = 9;
  const REMOVE_W = 26;
  const SHOW_LABEL_DIST = 50;
  const CLOCK_VB = 13;
  const ICON_SIZE = 13;
  const CROSS_VB = 10;
  const CROSS_SIZE = 9;

  const LINE_COLOR = 'rgba(41, 98, 255, 0.75)';
  const ICON_BG = '#2a2e39';
  const ICON_FG = '#d1d4dc';
  const BTN_HOVER_BG = '#50535e';

  const clockPlusIconPaths = [
    new Path2D('M5.34004 1.12254C4.7902 0.438104 3.94626 0 3 0C1.34315 0 0 1.34315 0 3C0 3.94626 0.438104 4.7902 1.12254 5.34004C1.04226 5.714 1 6.10206 1 6.5C1 9.36902 3.19675 11.725 6 11.9776V10.9725C3.75002 10.7238 2 8.81628 2 6.5C2 4.01472 4.01472 2 6.5 2C8.81628 2 10.7238 3.75002 10.9725 6H11.9776C11.9574 5.77589 11.9237 5.55565 11.8775 5.34011C12.562 4.79026 13.0001 3.9463 13.0001 3C13.0001 1.34315 11.6569 0 10.0001 0C9.05382 0 8.20988 0.438111 7.66004 1.12256C7.28606 1.04227 6.89797 1 6.5 1C6.10206 1 5.714 1.04226 5.34004 1.12254ZM4.28255 1.46531C3.93534 1.17484 3.48809 1 3 1C1.89543 1 1 1.89543 1 3C1 3.48809 1.17484 3.93534 1.46531 4.28255C2.0188 3.02768 3.02768 2.0188 4.28255 1.46531ZM8.71751 1.46534C9.97237 2.01885 10.9812 3.02774 11.5347 4.28262C11.8252 3.93541 12.0001 3.48812 12.0001 3C12.0001 1.89543 11.1047 1 10.0001 1C9.51199 1 9.06472 1.17485 8.71751 1.46534Z'),
    new Path2D('M7 7V4H8V8H5V7H7Z'),
    new Path2D('M10 8V10H8V11H10V13H11V11H13V10H11V8H10Z'),
  ];

  const clockIconPaths = [
    new Path2D('M5.11068 1.65894C3.38969 2.08227 1.98731 3.31569 1.33103 4.93171C0.938579 4.49019 0.700195 3.90868 0.700195 3.27148C0.700195 1.89077 1.81948 0.771484 3.2002 0.771484C3.9664 0.771484 4.65209 1.11617 5.11068 1.65894Z'),
    new Path2D('M12.5 3.37148C12.5 4.12192 12.1694 4.79514 11.6458 5.25338C11.0902 3.59304 9.76409 2.2857 8.09208 1.7559C8.55066 1.21488 9.23523 0.871484 10 0.871484C11.3807 0.871484 12.5 1.99077 12.5 3.37148Z'),
    new Path2D('M6.42896 11.4999C8.91424 11.4999 10.929 9.48522 10.929 6.99994C10.929 4.51466 8.91424 2.49994 6.42896 2.49994C3.94367 2.49994 1.92896 4.51466 1.92896 6.99994C1.92896 9.48522 3.94367 11.4999 6.42896 11.4999ZM6.00024 3.99994V6.99994H4.00024V7.99994H7.00024V3.99994H6.00024Z'),
    new Path2D('M4.08902 0.934101C4.4888 1.08621 4.83946 1.33793 5.11068 1.65894C5.06565 1.67001 5.02084 1.68164 4.97625 1.69382C4.65623 1.78123 4.34783 1.89682 4.0539 2.03776C3.16224 2.4653 2.40369 3.12609 1.8573 3.94108C1.64985 4.2505 1.47298 4.58216 1.33103 4.93171C1.05414 4.6202 0.853937 4.23899 0.760047 3.81771C0.720863 3.6419 0.700195 3.45911 0.700195 3.27148C0.700195 1.89077 1.81948 0.771484 3.2002 0.771484C3.51324 0.771484 3.81285 0.829023 4.08902 0.934101ZM12.3317 4.27515C12.4404 3.99488 12.5 3.69015 12.5 3.37148C12.5 1.99077 11.3807 0.871484 10 0.871484C9.66727 0.871484 9.34974 0.936485 9.05938 1.05448C8.68236 1.20769 8.35115 1.45027 8.09208 1.7559C8.43923 1.8659 8.77146 2.00942 9.08499 2.18265C9.96762 2.67034 10.702 3.39356 11.2032 4.26753C11.3815 4.57835 11.5303 4.90824 11.6458 5.25338C11.947 4.98973 12.1844 4.65488 12.3317 4.27515ZM9.18112 3.43939C8.42029 2.85044 7.46556 2.49994 6.42896 2.49994C3.94367 2.49994 1.92896 4.51466 1.92896 6.99994C1.92896 9.48522 3.94367 11.4999 6.42896 11.4999C8.91424 11.4999 10.929 9.48522 10.929 6.99994C10.929 5.55126 10.2444 4.26246 9.18112 3.43939ZM6.00024 3.99994H7.00024V7.99994H4.00024V6.99994H6.00024V3.99994Z'),
  ];

  const crossPath = new Path2D(
    'M9.35359 1.35359C9.11789 1.11789 8.88219 0.882187 8.64648 0.646484L5.00004 4.29293L1.35359 0.646484C1.11791 0.882212 0.882212 1.11791 0.646484 1.35359L4.29293 5.00004L0.646484 8.64648C0.882336 8.88204 1.11804 9.11774 1.35359 9.35359L5.00004 5.70714L8.64648 9.35359C8.88217 9.11788 9.11788 8.88217 9.35359 8.64649L5.70714 5.00004L9.35359 1.35359Z'
  );

  const measure = document.createElement('canvas').getContext('2d');
  function textWidth(text) {
    measure.font = '12px sans-serif';
    return Math.ceil(measure.measureText(text).width);
  }

  function newId() {
    return Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
  }

  const state = { code: '', name: '', alerts: [] };

  function keyOf(code) {
    return 'tv.alerts.' + code;
  }

  function loadAlerts(code) {
    if (!code) return [];
    let raw;
    try {
      raw = JSON.parse(localStorage.getItem(keyOf(code)) || '[]');
    } catch (_) {
      return [];
    }
    if (!Array.isArray(raw)) return [];
    return raw
      .filter(x => x && typeof x.price === 'number' && Number.isFinite(x.price))
      .map(x => ({ id: typeof x.id === 'string' ? x.id : newId(), price: x.price }))
      .sort((a, b) => b.price - a.price);
  }

  function saveAlerts() {
    if (!state.code) return;
    try {
      localStorage.setItem(keyOf(state.code), JSON.stringify(state.alerts));
    } catch (e) {
      console.error('预警保存失败', e);
    }
  }

  function priceText(price) {
    try {
      return TV.chart.candle.priceFormatter().format(price);
    } catch (_) {
      return Number(price).toFixed(2);
    }
  }

  let prim = null;

  function addAlert(price) {
    if (!Number.isFinite(price) || price <= 0) return;
    const p = Math.round(price * 1000) / 1000;
    state.alerts.push({ id: newId(), price: p });
    state.alerts.sort((a, b) => b.price - a.price);
    saveAlerts();
    if (prim) prim.refresh();
    renderPanel();
  }

  function removeAlert(id) {
    const before = state.alerts.length;
    state.alerts = state.alerts.filter(a => a.id !== id);
    if (state.alerts.length === before) return;
    saveAlerts();
    if (prim) prim.refresh();
    renderPanel();
  }

  function clearAlerts() {
    if (!state.alerts.length) return;
    state.alerts = [];
    saveAlerts();
    if (prim) prim.refresh();
    renderPanel();
  }

  function renderPanel() {
    if (!TV.panels || !TV.panels.section) return;
    const body = TV.panels.section('alerts', '价格预警',
      '<button class="mini" id="alerts-clear">清空</button>');

    const bars = TV.app && TV.app.state ? TV.app.state.bars : null;
    const close = bars && bars.length ? bars[bars.length - 1].close : null;
    const esc = TV.panels.esc;

    const hint = '<div class="dim" style="margin-bottom:4px">'
      + '图上移动鼠标，点右侧时钟 + 添加；拖动蓝色虚线可改价</div>';

    if (!state.alerts.length) {
      body.innerHTML = hint + '<div class="dim">暂无预警</div>';
    } else {
      body.innerHTML = hint + state.alerts.map(a => {
        let pct = '';
        let cls = 'dim';
        if (close) {
          const v = (a.price - close) / close * 100;
          pct = `${v > 0 ? '+' : ''}${v.toFixed(2)}%`;
          cls = v > 0 ? 'up' : v < 0 ? 'down' : 'dim';
        }
        return `<div class="watch-row">
          <span><b>${esc(priceText(a.price))}</b> <span class="${cls}">${pct}</span></span>
          <button class="mini" data-alert-del="${esc(a.id)}">×</button>
        </div>`;
      }).join('');
      body.querySelectorAll('button[data-alert-del]').forEach(btn => {
        btn.addEventListener('click', () => removeAlert(btn.dataset.alertDel));
      });
    }

    const clr = document.getElementById('alerts-clear');
    if (clr) clr.addEventListener('click', clearAlerts);
  }

  function setSymbol(code, name) {
    const changed = code !== state.code;
    state.code = code || '';
    state.name = name || '';
    if (changed) {
      state.alerts = loadAlerts(state.code);
      if (prim) prim.clearMouse();
    } else if (prim) {
      prim.refresh();
    }
    // 首次加载时面板还没创建，交给 main.js 在 load 末尾按固定顺序渲染
    if (document.getElementById('panel-alerts')) renderPanel();
  }

  class AlertsRenderer {
    constructor(owner) {
      this._owner = owner;
    }

    draw(target) {
      const o = this._owner;
      const data = o._data;
      if (!data || !o.param) return;

      target.useMediaCoordinateSpace(scope => {
        const ctx = scope.context;
        const W = scope.mediaSize.width;

        for (const a of data.alerts) {
          if (!Number.isFinite(a.y)) continue;

          ctx.save();
          ctx.strokeStyle = a.dragging ? '#2962ff' : LINE_COLOR;
          ctx.lineWidth = a.dragging ? 2 : 1;
          if (!a.dragging) ctx.setLineDash([4, 4]);
          ctx.beginPath();
          ctx.moveTo(0, a.y);
          ctx.lineTo(W - BUTTON_W - 1, a.y);
          ctx.stroke();
          ctx.restore();

          const x = W - BUTTON_W - 1;
          const yTop = a.y - ALERT_LABEL_H / 2;
          ctx.save();
          ctx.beginPath();
          ctx.roundRect(x, yTop, BUTTON_W, ALERT_LABEL_H, 2);
          ctx.fillStyle = ICON_BG;
          ctx.fill();
          drawIcon(ctx, clockIconPaths, x + 4, yTop + 2, ICON_SIZE, CLOCK_VB, ICON_FG);
          ctx.restore();
        }

        const drawing = TV.draw && TV.draw.state.mode !== 'none';
        const hovered = drawing ? null : data.alerts.find(a => a.showHover);
        if (data.button && !drawing && (!hovered || !hovered.hoverRemove)) {
          const x = W - BUTTON_W - 1;
          const yTop = data.button.y - BUTTON_H / 2;
          ctx.save();
          ctx.beginPath();
          ctx.roundRect(x, yTop, BUTTON_W, BUTTON_H, [2, 0, 0, 2]);
          ctx.fillStyle = data.button.hovering ? BTN_HOVER_BG : ICON_BG;
          ctx.fill();
          drawIcon(ctx, clockPlusIconPaths, x + 4, yTop + 4, ICON_SIZE, CLOCK_VB, '#ffffff');
          ctx.restore();
        }

        if (!hovered) return;
        const labelW = hovered.labelWidth;
        const labelX = (W - labelW) / 2;
        const labelY = hovered.y - CENTRE_LABEL_H / 2;
        const radius = 4;

        ctx.save();
        ctx.beginPath();
        ctx.roundRect(labelX, labelY, labelW, CENTRE_LABEL_H, radius);
        ctx.fillStyle = '#ffffff';
        ctx.fill();

        const removeX = labelX + labelW - REMOVE_W;
        if (hovered.hoverRemove) {
          ctx.save();
          ctx.beginPath();
          ctx.roundRect(removeX, labelY, REMOVE_W, CENTRE_LABEL_H, [0, radius, radius, 0]);
          ctx.fillStyle = '#f0f3fa';
          ctx.fill();
          ctx.restore();
        }

        ctx.fillStyle = '#f1f3fb';
        ctx.fillRect(removeX, labelY, 1, CENTRE_LABEL_H);

        ctx.beginPath();
        ctx.roundRect(labelX + 0.5, labelY + 0.5, labelW - 1, CENTRE_LABEL_H - 1, radius);
        ctx.strokeStyle = '#131722';
        ctx.lineWidth = 1;
        ctx.stroke();

        ctx.fillStyle = '#131722';
        ctx.font = '12px sans-serif';
        ctx.textBaseline = 'middle';
        ctx.fillText(hovered.text, labelX + CENTRE_LABEL_PAD, hovered.y);

        drawIcon(
          ctx, [crossPath],
          removeX + (REMOVE_W - CROSS_SIZE) / 2,
          hovered.y - CROSS_SIZE / 2,
          CROSS_SIZE, CROSS_VB, '#131722'
        );
        ctx.restore();
      });
    }
  }

  function drawIcon(ctx, paths, x, y, size, box, color) {
    ctx.save();
    ctx.translate(x, y);
    const s = size / box;
    ctx.scale(s, s);
    ctx.fillStyle = color;
    for (const p of paths) {
      ctx.beginPath();
      ctx.fill(p, 'evenodd');
    }
    ctx.restore();
  }

  class AlertsPaneView {
    constructor(owner) {
      this._owner = owner;
      this._renderer = new AlertsRenderer(owner);
    }
    zOrder() { return 'top'; }
    renderer() { return this._renderer; }
  }

  class AlertsPrimitive {
    constructor() {
      this.param = null;
      this.series = null;
      this.pane = null;
      this.host = null;
      this._view = new AlertsPaneView(this);
      this._mouse = null;
      this._down = null;
      this._data = null;
      this._pointer = false;
      this._hoverID = '';
      this._cursor = 'pointer';
      this._drag = null;
      this._suppressClick = false;
      this._locate = null;
    }

    attached(param) {
      this.param = param;
      this.series = param.series;
      this._bindMouse();
      this.refresh();
    }

    detached() {
      this._unbindMouse();
      this.param = null;
      this.series = null;
      this._data = null;
    }

    paneViews() { return [this._view]; }

    hitTest() {
      if (!this._pointer || (TV.draw && TV.draw.state.mode !== 'none')) return null;
      return {
        cursorStyle: this._cursor || 'pointer',
        externalId: 'tv-price-alerts',
        zOrder: 'top',
      };
    }

    refresh() {
      if (this.param) this.param.requestUpdate();
    }

    clearMouse() {
      this._mouse = null;
      this._down = null;
      this._drag = null;
      this.refresh();
    }

    _bindMouse() {
      this._unbindMouse();
      const host = this.param.chart.chartElement();
      const pane = this.param.chart.panes()[0].getHTMLElement();
      if (!host || !pane) return;
      this.host = host;
      this.pane = pane;

      const locate = ev => {
        const r = pane.getBoundingClientRect();
        const x = ev.clientX - r.left;
        const y = ev.clientY - r.top;
        if (x < 0 || x > r.width || y < 0 || y > r.height) return null;
        return { x, y };
      };
      this._locate = locate;

      this._onMove = ev => {
        if (TV.draw && TV.draw.state.mode !== 'none') {
          this._down = null;
          if (this._mouse) { this._mouse = null; this.refresh(); }
          return;
        }
        const pos = locate(ev);
        if (!pos) {
          if (this._mouse) { this._mouse = null; this.refresh(); }
          return;
        }
        if (this._mouse && this._mouse.x === pos.x && this._mouse.y === pos.y) return;
        this._mouse = pos;
        this.refresh();
      };
      this._onLeave = () => {
        this._down = null;
        if (!this._mouse) return;
        this._mouse = null;
        this.refresh();
      };
      this._onDown = ev => {
        if (ev.button !== 0) return;
        if (TV.draw && TV.draw.state.mode !== 'none') return;
        this._down = { x: ev.clientX, y: ev.clientY };
        const pos = locate(ev);
        if (!pos) return;
        this._mouse = pos;
        this._calculateData();
        if (this._hoverID) return;
        const hit = (this._data && this._data.alerts || [])
          .find(a => Number.isFinite(a.y) && Math.abs(pos.y - a.y) <= 6);
        if (!hit) return;

        this._drag = { id: hit.id, moved: false };
        this._down = null;
        ev.preventDefault();
        ev.stopPropagation();
        host.style.setProperty('cursor', 'ns-resize', 'important');
        this.refresh();
      };
      this._onWinMove = ev => {
        if (!this._drag || !this.series || !this.pane) return;
        if (this.host) this.host.style.setProperty('cursor', 'ns-resize', 'important');
        const r = this.pane.getBoundingClientRect();
        const y = Math.min(Math.max(ev.clientY - r.top, 0), r.height - 1);
        const raw = this.series.coordinateToPrice(y);
        if (raw == null || !Number.isFinite(raw)) return;
        const price = Math.round(raw * 1000) / 1000;
        if (!(price > 0)) return;
        const alert = state.alerts.find(a => a.id === this._drag.id);
        if (!alert || alert.price === price) return;
        alert.price = price;
        this._drag.moved = true;
        state.alerts.sort((a, b) => b.price - a.price);
        this._mouse = { x: ev.clientX - r.left, y };
        this.refresh();
      };
      this._onWinUp = () => {
        if (!this._drag) return;
        const moved = this._drag.moved;
        this._drag = null;
        if (this.host) this.host.style.removeProperty('cursor');
        if (!moved) {
          this.refresh();
          return;
        }
        saveAlerts();
        this._suppressClick = true;
        setTimeout(() => { this._suppressClick = false; }, 0);
        renderPanel();
        this.refresh();
      };
      this._onClick = ev => {
        if (this._suppressClick) {
          this._suppressClick = false;
          return;
        }
        if (TV.draw && TV.draw.state.mode !== 'none') {
          this._down = null;
          return;
        }
        if (this._down) {
          const moved = Math.abs(ev.clientX - this._down.x) > 4
            || Math.abs(ev.clientY - this._down.y) > 4;
          this._down = null;
          if (moved) return;
        }
        const pos = locate(ev);
        if (!pos) return;
        this._mouse = pos;
        this._calculateData();
        if (this._hoverID) {
          this._remove(this._hoverID);
        } else if (this._data && this._data.button && this._data.button.hovering) {
          this._add(pos.y);
        } else {
          this.refresh();
        }
      };

      host.addEventListener('mousemove', this._onMove);
      host.addEventListener('mouseleave', this._onLeave);
      host.addEventListener('mousedown', this._onDown, true);
      host.addEventListener('click', this._onClick);
      window.addEventListener('mousemove', this._onWinMove);
      window.addEventListener('mouseup', this._onWinUp);
    }

    _unbindMouse() {
      if (this._onWinMove) window.removeEventListener('mousemove', this._onWinMove);
      if (this._onWinUp) window.removeEventListener('mouseup', this._onWinUp);
      if (!this.host) return;
      if (this._onMove) this.host.removeEventListener('mousemove', this._onMove);
      if (this._onLeave) this.host.removeEventListener('mouseleave', this._onLeave);
      if (this._onDown) this.host.removeEventListener('mousedown', this._onDown, true);
      if (this._onClick) this.host.removeEventListener('click', this._onClick);
      this.host = null;
      this.pane = null;
      this._locate = null;
      this._onMove = this._onLeave = this._onDown = this._onClick = null;
      this._onWinMove = this._onWinUp = null;
    }

    _add(y) {
      if (!this.series) return;
      const price = this.series.coordinateToPrice(y);
      if (price == null || !Number.isFinite(price)) return;
      const rounded = Math.round(price * 1000) / 1000;
      state.alerts.push({ id: newId(), price: rounded });
      state.alerts.sort((a, b) => b.price - a.price);
      saveAlerts();
      this.refresh();
      renderPanel();
    }

    _remove(id) {
      removeAlert(id);
    }

    updateAllViews() {
      this._calculateData();
    }

    _calculateData() {
      if (!this.series) { this._data = null; this._pointer = false; return; }
      const mouse = this._mouse;
      const W = this.param ? this.param.chart.timeScale().width() : 0;
      const drawing = TV.draw && TV.draw.state.mode !== 'none';
      this._hoverID = '';
      this._pointer = false;
      this._cursor = 'pointer';
      this._data = { alerts: [], button: null };

      const alerts = state.alerts.map(a => {
        const y = this.series.priceToCoordinate(a.price);
        return {
          id: a.id,
          price: a.price,
          y: y == null ? NaN : y,
          dragging: !!(this._drag && this._drag.id === a.id),
          showHover: false,
          hoverRemove: false,
          text: '',
          labelWidth: 0,
        };
      });

      if (mouse && !drawing && !this._drag) {
        let closest = -1;
        let closestDist = Infinity;
        for (let i = 0; i < alerts.length; i++) {
          const y = alerts[i].y;
          if (!Number.isFinite(y)) continue;
          const d = Math.abs(mouse.y - y);
          if (d < closestDist) { closestDist = d; closest = i; }
        }

        let onLine = false;
        if (closest >= 0 && closestDist < SHOW_LABEL_DIST) {
          const a = alerts[closest];
          const text = `${state.name || state.code} 触及 ${priceText(a.price)}`;
          const labelW = CENTRE_LABEL_PAD * 2 + REMOVE_W + textWidth(text);
          a.text = text;
          a.labelWidth = labelW;
          a.showHover = true;
          onLine = closestDist <= 6;

          const removeCentreX = (W + labelW - REMOVE_W) / 2;
          if (
            Math.abs(mouse.y - a.y) <= CENTRE_LABEL_H / 2 &&
            Math.abs(mouse.x - removeCentreX) <= REMOVE_W / 2
          ) {
            a.hoverRemove = true;
            this._hoverID = a.id;
            this._pointer = true;
          }
        }

        if (!this._hoverID && onLine) {
          this._pointer = true;
          this._cursor = 'ns-resize';
        }

        const xRel = W - mouse.x;
        const inStrip = xRel >= 1 && xRel < BUTTON_W;
        this._data.button = { y: mouse.y, hovering: inStrip };
        if (inStrip) {
          this._pointer = true;
          this._cursor = 'pointer';
        }
      }

      this._data.alerts = alerts;
    }
  }

  function init() {
    prim = new AlertsPrimitive();
    TV.chart.candle.attachPrimitive(prim);
  }

  TV.alerts = {
    init, setSymbol, add: addAlert,
    remove: removeAlert, clear: clearAlerts, renderPanel,
  };
})();
