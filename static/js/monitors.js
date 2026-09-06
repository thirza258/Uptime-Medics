// Monitor features shared by the landing page and the dashboard: the create form and the
// check-history modal.

import { api } from './api.js';
import { $, $$, esc, formatInterval, methodBadge, openModal, parseHeadersField, statusBadge, toast } from './ui.js';

/**
 * Wire the shared "Monitor any endpoint" form.
 * @param {{ isPrivate: boolean, onCreated?: (monitor: object) => void }} options
 */
export function initMonitorForm({ isPrivate, onCreated }) {
    const form = $('#monitor-form');
    if (!form) return;
    const hint = $('#create-hint');
    if (hint) hint.textContent = isPrivate ? 'Monitors you create are private to your account' : 'Anonymous or registered – no sign-up needed';

    form.addEventListener('submit', async (event) => {
        event.preventDefault();
        const button = $('#submit-btn');
        const url = $('#target-url').value.trim();
        if (!url) { toast('Please enter a URL to monitor.', 'error'); $('#target-url').focus(); return; }
        let headers;
        try { headers = parseHeadersField($('#target-headers').value); } catch (error) {
            toast(error.message, 'error');
            $('#advanced').open = true;
            return;
        }
        const payload = {
            url,
            method: $('#target-method').value,
            name: $('#target-name').value.trim() || null,
            alert_email: $('#target-email').value.trim() || null,
            body: $('#target-payload').value || null,
            headers,
            check_interval_seconds: Number($('#target-interval').value),
            timeout_seconds: Number($('#target-timeout').value) || 10,
            expected_status_code: Number($('#target-expected').value) || 200,
        };
        button.disabled = true;
        button.textContent = 'Deploying…';
        try {
            const monitor = await api('/api/monitors', { method: 'POST', body: payload });
            toast('Monitor deployed. First check runs within seconds.', 'success');
            form.reset();
            $('#target-expected').value = 200;
            $('#target-timeout').value = 10;
            const panel = $('#result-panel');
            panel.hidden = false;
            const where = monitor.is_anonymous ? ' and is visible in the community watch.' : ' and is private to your account.';
            panel.innerHTML = `<span>${statusBadge(monitor)} ${methodBadge(monitor.method)} <strong>${esc(monitor.name)}</strong> is now being checked every ${esc(formatInterval(monitor.check_interval_seconds))}${where}</span>
                <button type="button" class="btn-secondary" data-logs="${esc(monitor.id)}" data-name="${esc(monitor.name)}">View checks</button>`;
            if (onCreated) onCreated(monitor);
        } catch (error) {
            toast(error.message || 'Submission failed', 'error');
        } finally {
            button.disabled = false;
            button.textContent = 'Start monitoring';
        }
    });
}

/** Open the check-history modal for a monitor. */
export async function openLogs(id, name) {
    $('#logs-title').textContent = `Checks · ${name || 'monitor'}`;
    const content = $('#logs-content');
    content.innerHTML = '<p class="empty">Loading…</p>';
    openModal('#logs-modal');
    let logs;
    try { logs = await api(`/api/monitors/${encodeURIComponent(id)}/logs`); } catch (error) {
        content.innerHTML = `<p class="empty">${esc(error.message)}</p>`;
        return;
    }
    if (!logs.length) {
        content.innerHTML = '<p class="empty">No checks recorded yet – the first probe runs within seconds of creation.</p>';
        return;
    }
    const chronological = [...logs].reverse();
    const max = Math.max(1, ...chronological.map((l) => l.response_time_ms));
    const okCount = logs.filter((l) => l.is_successful).length;
    const avg = Math.round(logs.reduce((sum, l) => sum + l.response_time_ms, 0) / logs.length);
    content.innerHTML = `
        <div class="legend">
            <span><span class="dot ok"></span>Success</span>
            <span><span class="dot fail"></span>Failure</span>
            <span>Last ${logs.length} checks · ${okCount}/${logs.length} successful · avg ${avg} ms</span>
        </div>
        <div class="bars" aria-hidden="true">${chronological.map((l) => `<div class="bar ${l.is_successful ? '' : 'fail'}" style="height:${Math.max(4, Math.round((l.response_time_ms / max) * 100))}%" title="${esc(new Date(l.checked_at).toLocaleString())} · ${l.response_time_ms} ms${l.error_message ? ' · ' + esc(l.error_message) : ''}"></div>`).join('')}</div>
        <div class="table-wrap"><table>
            <thead><tr><th>Checked</th><th>Result</th><th>HTTP</th><th class="num">Latency</th><th>Details</th></tr></thead>
            <tbody>${logs.map((l) => `<tr>
                <td class="mono" title="${esc(l.checked_at)}">${esc(new Date(l.checked_at).toLocaleString())}</td>
                <td class="${l.is_successful ? 'ok-text' : 'fail-text'}">${l.is_successful ? 'OK' : 'FAIL'}</td>
                <td class="mono">${l.status_code ?? '—'}</td>
                <td class="num">${l.response_time_ms} ms</td>
                <td class="mono muted">${esc(l.error_message || '')}</td>
            </tr>`).join('')}</tbody>
        </table></div>`;
}

/** Open check histories from any `[data-logs]` element, keyboard-activated rows and `#monitor=<id>` links. */
export function initLogsInteractions() {
    document.addEventListener('click', (event) => {
        const target = event.target.closest('[data-logs]');
        if (target) openLogs(target.dataset.logs, target.dataset.name);
    });
    document.addEventListener('keydown', (event) => {
        if (event.key !== 'Enter' && event.key !== ' ') return;
        const row = event.target.closest('.monitor-row[data-logs]');
        if (row) { event.preventDefault(); openLogs(row.dataset.logs, row.dataset.name); }
    });
    const handleHash = () => {
        const match = location.hash.match(/^#monitor=([\w-]+)$/);
        if (match) openLogs(match[1], 'monitor');
    };
    window.addEventListener('hashchange', handleHash);
    handleHash();
    // Keep unused import warnings away in strict tooling.
    void $$;
}
