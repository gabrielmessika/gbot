// ── gbot dashboard — SSE-driven single page ──

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

// ── State ──
let lastTs = 0;
let sseConnected = false;

// ── SSE connection ──
function connectSSE() {
    const evtSource = new EventSource('/api/stream');

    evtSource.onopen = () => {
        sseConnected = true;
        updateLiveDot('green');
    };

    evtSource.onmessage = (e) => {
        try {
            const data = JSON.parse(e.data);
            lastTs = data.ts || Date.now();
            render(data);
        } catch (err) {
            console.error('SSE parse error:', err);
        }
    };

    evtSource.onerror = () => {
        sseConnected = false;
        updateLiveDot('red');
        // EventSource reconnects automatically
    };
}

// ── Render full snapshot ──
function render(d) {
    renderHeader(d);
    renderBooks(d.books || {});
    renderPositions(d.positions || [], d.pending_orders || []);
    renderMetrics(d.metrics || {});
    renderEvents(d.events || []);
}

// ── Header ──
function renderHeader(d) {
    $('#equity').textContent = '$' + fmt(d.equity, 2);

    const pnlEl = $('#daily-pnl');
    pnlEl.textContent = (d.daily_pnl >= 0 ? '+' : '') + '$' + fmt(d.daily_pnl, 2);
    pnlEl.className = 'value ' + pnlClass(d.daily_pnl);

    const ddEl = $('#drawdown');
    ddEl.textContent = fmt(d.drawdown_pct, 2) + '%';
    ddEl.className = 'value ' + (d.drawdown_pct > 10 ? 'color-red' : d.drawdown_pct > 5 ? 'color-orange' : 'color-green');

    const posCount = (d.positions || []).length;
    $('#pos-count').textContent = posCount;

    updateLiveDot('green');
}

// ── Books ──
function renderBooks(books) {
    const container = $('#books-container');
    const coins = Object.keys(books).sort();

    if (coins.length === 0) {
        container.innerHTML = '<div class="empty-state">En attente de données...</div>';
        return;
    }

    let html = '';
    for (const coin of coins) {
        const b = books[coin];
        const regime = b.regime || 'Unknown';
        const spreadClass = b.spread_bps < 3 ? 'color-green' : b.spread_bps < 8 ? 'color-orange' : 'color-red';
        const toxClass = b.toxicity < 0.4 ? 'color-green' : b.toxicity < 0.7 ? 'color-orange' : 'color-red';
        const aloOk = regime === 'QuietTight' || regime === 'ActiveHealthy' || regime === 'QuietThin';
        const imb = b.imbalance_top5 || 0;
        const imbPct = Math.min(Math.abs(imb) * 50, 50); // scale to %

        html += `
        <div class="book-card">
            <div class="book-card-header">
                <span class="book-coin">${coin}</span>
                <span class="regime-badge regime-${regime}">${regimeLabel(regime)}</span>
            </div>
            <div class="book-stats">
                <div class="book-stat">
                    <span class="label">Spread</span>
                    <span class="value ${spreadClass}">${fmt(b.spread_bps, 1)} bps</span>
                </div>
                <div class="book-stat">
                    <span class="label">Micro-px</span>
                    <span class="value">${fmtSigned(b.micro_price_vs_mid_bps, 1)} bps</span>
                </div>
                <div class="book-stat">
                    <span class="label">Toxicity</span>
                    <span class="value ${toxClass}">${fmt(b.toxicity, 2)}</span>
                </div>
                <div class="book-stat">
                    <span class="label">Imb top5</span>
                    <span class="value">${fmtSigned(imb * 100, 0)}%</span>
                </div>
                <div class="imbalance-bar-wrapper">
                    <div class="imbalance-bar">
                        <div class="imbalance-bar-fill ${imb >= 0 ? 'positive' : 'negative'}"
                             style="width:${imbPct}%;${imb < 0 ? 'right:50%;left:auto;' : ''}"></div>
                    </div>
                </div>
            </div>
            <div class="book-stat" style="margin-top:4px;">
                <span class="label">Toxicity</span>
                <div class="tox-gauge">
                    <div class="tox-gauge-fill" style="width:${Math.min(b.toxicity * 100, 100)}%;background:${toxColor(b.toxicity)}"></div>
                </div>
            </div>
            <div class="alo-eligible ${aloOk ? 'alo-yes' : 'alo-no'}">
                ALO: ${aloOk ? '✓' : '✗'}
            </div>
        </div>`;
    }

    container.innerHTML = html;
}

// ── Positions + Pending Orders ──
function renderPositions(positions, pendingOrders) {
    const container = $('#positions-container');

    if (positions.length === 0 && pendingOrders.length === 0) {
        container.innerHTML = '<div class="empty-state">Aucune position</div>';
        return;
    }

    let html = '';

    // Open positions
    for (const p of positions) {
        const dir = p.direction.toUpperCase();
        const dirClass = dir === 'LONG' ? 'long' : 'short';
        const pnlCls = p.pnl_pct > 0 ? 'pnl-positive' : p.pnl_pct < 0 ? 'pnl-negative' : 'pnl-zero';

        html += `
        <div class="position-card ${dirClass}">
            <div class="position-header">
                <div>
                    <span class="position-coin">${p.coin}</span>
                    <span class="position-direction direction-${dirClass}">${dir}</span>
                </div>
                <span class="position-pnl ${pnlCls}">${fmtSigned(p.pnl_pct, 2)}% ($${fmtSigned(p.pnl_usd, 2)})</span>
            </div>
            <div class="position-details">
                <div class="position-detail">
                    <span class="label">Entry</span>
                    <span class="value">${fmtPrice(p.entry_price)}</span>
                </div>
                <div class="position-detail">
                    <span class="label">Current</span>
                    <span class="value">${fmtPrice(p.current_price)}</span>
                </div>
                <div class="position-detail">
                    <span class="label">Elapsed</span>
                    <span class="value">${fmtDuration(p.elapsed_s)}</span>
                </div>
                <div class="position-detail">
                    <span class="label">SL</span>
                    <span class="value color-red">${fmtPrice(p.sl)}</span>
                </div>
                <div class="position-detail">
                    <span class="label">TP</span>
                    <span class="value color-green">${fmtPrice(p.tp)}</span>
                </div>
                <div class="position-detail">
                    <span class="label">BE</span>
                    <span class="value">${p.break_even_applied ? '✓' : '—'}</span>
                </div>
            </div>
        </div>`;
    }

    // Pending orders
    for (const o of pendingOrders) {
        const dir = o.direction.toUpperCase();
        html += `
        <div class="pending-card">
            <div class="pending-info">
                <span class="pending-coin">${o.coin}</span>
                <span class="position-direction direction-${dir === 'LONG' ? 'long' : 'short'}">${dir}</span>
                <span>@ ${fmtPrice(o.price)}</span>
            </div>
            <span class="pending-timer">${o.placed_s_ago}s / ${o.max_wait_s}s</span>
        </div>`;
    }

    container.innerHTML = html;
}

// ── Metrics ──
function renderMetrics(m) {
    setText('#m-fill-rate', fmt(m.maker_fill_rate_1h * 100, 0) + '%');
    setText('#m-adverse', fmt(m.adverse_selection_rate_1h * 100, 0) + '%');
    setText('#m-spread-cap', fmt(m.spread_capture_bps_session, 1) + ' bps');
    setText('#m-lag', fmt(m.queue_lag_ms_p95, 0) + ' ms');
    setText('#m-reconnects', m.ws_reconnects_today);
    setText('#m-killswitch', m.kill_switch_count);
}

// ── Events ──
function renderEvents(events) {
    const container = $('#events-container');

    if (events.length === 0) {
        container.innerHTML = '<div class="empty-state">En attente d\'événements...</div>';
        return;
    }

    let html = '';
    // Show newest first
    for (let i = events.length - 1; i >= 0; i--) {
        const ev = events[i];
        const time = fmtTime(ev.ts);
        const cls = 'event-' + (ev.event_type || 'system');

        html += `<div class="event-line"><span class="event-ts">[${time}]</span><span class="${cls}">${escapeHtml(ev.message)}</span></div>`;
    }

    container.innerHTML = html;

    // Auto-scroll to top (newest)
    container.scrollTop = 0;
}

// ── Helpers ──

function updateLiveDot(color) {
    const dot = $('#live-dot');
    dot.className = 'dot dot-' + color;
    dot.title = color === 'green' ? 'Connected' : color === 'red' ? 'Disconnected' : 'Connecting...';
}

function fmt(v, dec) {
    if (v == null || isNaN(v)) return '—';
    return Number(v).toFixed(dec);
}

function fmtSigned(v, dec) {
    if (v == null || isNaN(v)) return '—';
    const n = Number(v);
    return (n >= 0 ? '+' : '') + n.toFixed(dec);
}

function fmtPrice(v) {
    if (v == null || v === 0) return '—';
    // Auto-detect decimals: > 1000 → 1, > 10 → 2, > 1 → 3, else 4
    const n = Math.abs(v);
    const dec = n >= 1000 ? 1 : n >= 10 ? 2 : n >= 1 ? 3 : 4;
    return Number(v).toFixed(dec);
}

function fmtDuration(secs) {
    if (secs == null || secs < 0) return '—';
    if (secs < 60) return secs + 's';
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    return m + 'm' + (s > 0 ? s + 's' : '');
}

function fmtTime(tsMs) {
    const d = new Date(tsMs);
    return d.toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function pnlClass(v) {
    if (v > 0) return 'color-green';
    if (v < 0) return 'color-red';
    return 'color-muted';
}

function toxColor(v) {
    if (v < 0.4) return 'var(--green)';
    if (v < 0.7) return 'var(--orange)';
    return 'var(--red)';
}

function regimeLabel(r) {
    const map = {
        'QuietTight': 'Quiet Tight',
        'ActiveHealthy': 'Active Healthy',
        'QuietThin': 'Quiet Thin',
        'ActiveToxic': 'Active Toxic',
        'WideSpread': 'Wide Spread',
        'LowSignal': 'Low Signal',
        'NewslikeChaos': 'Chaos',
        'DoNotTrade': 'DO NOT TRADE',
    };
    return map[r] || r;
}

function setText(sel, val) {
    const el = $(sel);
    if (el) el.textContent = val ?? '—';
}

function escapeHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}

// ── Stale detection ──
// If no SSE data for > 5s, show red indicator
setInterval(() => {
    if (!sseConnected) return;
    const age = Date.now() - lastTs;
    if (age > 5000) {
        updateLiveDot('red');
    }
}, 2000);

// ── Init ──
connectSSE();
