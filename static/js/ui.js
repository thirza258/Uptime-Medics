// Shared UI helpers: DOM shortcuts, escaping and formatting, badges, monitor rows, toasts,
// modals, browser storage and navigation.

export const $ = (selector, root = document) => root.querySelector(selector);
export const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

export function esc(value) {
    return String(value ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

export function truncate(text, max) {
    text = String(text ?? '');
    return text.length > max ? text.slice(0, max) + '…' : text;
}

export function timeAgo(iso) {
    if (!iso) return 'queued';
    const seconds = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
    if (seconds < 5) return 'just now';
    if (seconds < 60) return `${seconds}s ago`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
    return `${Math.floor(seconds / 86400)}d ago`;
}

export function formatInterval(seconds) {
    if (seconds % 3600 === 0) return `${seconds / 3600}h`;
    if (seconds % 60 === 0) return `${seconds / 60}m`;
    return `${seconds}s`;
}

/** Compact label for a failure message, e.g. "HTTP 502", "TIMEOUT", "DNS". */
export function shortError(message) {
    if (!message) return 'ERROR';
    if (message.startsWith('TIMEOUT')) return 'TIMEOUT';
    const status = message.match(/^Status (\d{3})/);
    if (status) return `HTTP ${status[1]}`;
    if (/dns|lookup|resolve/i.test(message)) return 'DNS';
    if (/certificate|tls|ssl/i.test(message)) return 'TLS';
    if (/refused|connect/i.test(message)) return 'CONN';
    if (/blocked/i.test(message)) return 'BLOCKED';
    return 'ERROR';
}

export function statusBadge(monitor) {
    if (monitor.is_active === false) return '<span class="badge status-paused">PAUSED</span>';
    const status = String(monitor.current_status || 'PENDING');
    return `<span class="badge status-${esc(status.toLowerCase())}">${esc(status)}</span>`;
}

export function methodBadge(method) {
    return `<span class="badge badge-${esc(String(method).toLowerCase())}">${esc(method)}</span>`;
}

/** One row of the community watch / recent submissions lists. Click opens the check history. */
export function monitorRow(monitor) {
    const latency = monitor.response_time_ms != null ? `${monitor.response_time_ms} ms` : '—';
    const isDown = monitor.current_status === 'DOWN';
    const detail = isDown ? shortError(monitor.last_error) : latency;
    return `<div class="monitor-row juggle-enter" role="button" tabindex="0" data-logs="${esc(monitor.id)}" data-name="${esc(monitor.name)}" aria-label="Show checks for ${esc(monitor.name)}">
        <div class="monitor-main">
            ${methodBadge(monitor.method)}
            <div class="monitor-text">
                <div class="monitor-name">${esc(monitor.name)}</div>
                <div class="url-truncate" title="${esc(monitor.url)}">${esc(truncate(monitor.url, 40))}</div>
            </div>
        </div>
        <div class="monitor-meta">
            <span class="latency ${isDown ? 'latency-error' : ''}" title="${esc(monitor.last_error || latency)}">${esc(detail)}</span>
            ${statusBadge(monitor)}
            <span class="checked-at" title="${esc(monitor.last_checked_at || '')}">${esc(timeAgo(monitor.last_checked_at))}</span>
        </div>
    </div>`;
}

export function toast(message, kind = 'info') {
    const container = $('#toasts');
    if (!container) return;
    const element = document.createElement('div');
    element.className = `toast ${kind}`;
    element.textContent = message;
    container.appendChild(element);
    setTimeout(() => element.remove(), 4500);
}

export function openModal(id) {
    const modal = $(id);
    if (!modal) return;
    modal.hidden = false;
    const focusable = modal.querySelector('input, select, textarea, button:not(.close)');
    if (focusable) focusable.focus();
}

export function closeModals() {
    $$('.modal-backdrop').forEach((modal) => { modal.hidden = true; });
    if (location.hash.startsWith('#monitor=')) history.replaceState(null, '', location.pathname);
}

/** Wire backdrop clicks, close buttons and the Escape key for every modal on the page. */
export function initModals() {
    $$('.modal-backdrop').forEach((backdrop) => {
        backdrop.addEventListener('click', (event) => { if (event.target === backdrop) closeModals(); });
        $$('[data-close]', backdrop).forEach((button) => button.addEventListener('click', closeModals));
    });
    document.addEventListener('keydown', (event) => { if (event.key === 'Escape') closeModals(); });
}

/** Parse the optional "custom headers" textarea; returns null when empty, throws on bad input. */
export function parseHeadersField(text) {
    const trimmed = text.trim();
    if (!trimmed) return null;
    let parsed;
    try { parsed = JSON.parse(trimmed); } catch (_) {
        throw new Error('Custom headers must be a valid JSON object, e.g. {"Authorization": "Bearer token"}');
    }
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Custom headers must be a JSON object');
    return parsed;
}

/** localStorage with graceful fallback when storage is unavailable. */
export const storage = {
    get(key, fallback) {
        try {
            const raw = localStorage.getItem(key);
            return raw === null ? fallback : JSON.parse(raw);
        } catch (_) { return fallback; }
    },
    set(key, value) {
        try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) { /* storage unavailable */ }
    },
};

/** Navigate to a same-origin URL. Tests can intercept via `window.__testNavigate`. */
export function navigate(url) {
    if (typeof window.__testNavigate === 'function') return window.__testNavigate(url);
    window.location.assign(url);
}
