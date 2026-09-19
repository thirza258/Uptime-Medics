// SQLite-backed summaries and charts for the full selected period.

import { api } from './api.js';
import { $, $$, esc, openModal } from './ui.js';

const LABELS = { '24h': '24 hours', '7d': '7 days', '30d': '30 days' };
let monitorId = null;
let selectedRange = '24h';
let requestVersion = 0;

const timestamp = (value) => new Date(value).toLocaleString();
const percentage = (value) => value == null ? 'No data' : `${value.toFixed(2)}%`;

function bucketDescription(bucket) {
    const interval = `${timestamp(bucket.start)} – ${timestamp(bucket.end)}`;
    return bucket.total_checks
        ? `${interval}: ${percentage(bucket.uptime_percent)} uptime · ${bucket.successful_checks}/${bucket.total_checks} successful · ${bucket.failed_checks} failed · avg ${Math.round(bucket.avg_response_time_ms)} ms`
        : `${interval}: No checks recorded`;
}

function renderHistory(history) {
    const summary = history.summary;
    const logs = history.recent_checks;
    const rangeHours = (new Date(history.end) - new Date(history.start)) / 3600000;
    const retentionNote = history.retention_hours != null && history.retention_hours < rangeHours
        ? `<p class="history-notice">This server retains ${esc(history.retention_hours)} hours of checks for this monitor. Older checks may have been removed.</p>` : '';
    return `
        <p class="hint history-window">${esc(timestamp(history.start))} – ${esc(timestamp(history.end))} · local time</p>
        <dl class="history-stats">
            <div><dt>Uptime</dt><dd>${percentage(summary.uptime_percent)}</dd></div>
            <div><dt>Total checks</dt><dd>${summary.total_checks.toLocaleString()}</dd></div>
            <div><dt>Failed checks</dt><dd class="${summary.failed_checks ? 'fail-text' : ''}">${summary.failed_checks.toLocaleString()}</dd></div>
            <div><dt>Avg latency</dt><dd>${summary.avg_response_time_ms == null ? '—' : `${Math.round(summary.avg_response_time_ms)} <small>ms</small>`}</dd></div>
        </dl>
        <p class="hint history-explainer">Uptime = successful checks ÷ recorded checks. Gaps, pauses, and time before monitoring began are not counted as uptime or downtime.</p>
        ${retentionNote}
        <div class="legend">
            <span><span class="dot ok"></span>Successful checks</span>
            <span><span class="dot fail"></span>Failed checks</span>
            <span><span class="dot no-data"></span>No checks</span>
            <span>${history.bucket_seconds / 3600}-hour intervals</span>
        </div>
        <div class="history-chart" role="group" aria-label="Uptime for the last ${esc(LABELS[history.range])}; select an interval for details">
            ${history.buckets.map((bucket, index) => `<button type="button" class="history-bar ${bucket.total_checks ? '' : 'no-data'}" data-history-bucket="${index}" aria-label="${esc(bucketDescription(bucket))}" title="${esc(bucketDescription(bucket))}">
                ${bucket.total_checks ? `<span class="history-success" style="height:${bucket.uptime_percent}%"></span>` : ''}
            </button>`).join('')}
        </div>
        <div class="history-axis"><span>${esc(timestamp(history.start))}</span><span>${esc(timestamp(history.end))}</span></div>
        <p class="hint history-detail" id="history-bucket-detail" role="status">Select an interval to inspect its checks.</p>
        ${summary.total_checks ? `<p class="hint history-observed">Recorded checks in this period: ${esc(timestamp(summary.first_checked_at))} – ${esc(timestamp(summary.last_checked_at))}.</p>` : '<p class="empty" role="status">No checks recorded in this period. New monitors build history as checks run; try another period for older results.</p>'}
        ${logs.length ? `<h4 class="history-checks-title">Latest ${logs.length} checks in this period</h4>
        <div class="table-wrap"><table>
            <thead><tr><th>Checked</th><th>Result</th><th>HTTP</th><th class="num">Latency</th><th>Details</th></tr></thead>
            <tbody>${logs.map((log) => `<tr>
                <td class="mono" title="${esc(log.checked_at)}">${esc(timestamp(log.checked_at))}</td>
                <td class="${log.is_successful ? 'ok-text' : 'fail-text'}">${log.is_successful ? 'OK' : 'FAIL'}</td>
                <td class="mono">${log.status_code ?? '—'}</td>
                <td class="num">${log.response_time_ms} ms</td>
                <td class="mono muted">${esc(log.error_message || '')}</td>
            </tr>`).join('')}</tbody>
        </table></div>` : ''}`;
}

async function loadHistory(range) {
    selectedRange = range;
    const version = ++requestVersion;
    const content = $('#logs-content');
    $$('[data-history-range]').forEach((button) => {
        button.setAttribute('aria-pressed', String(button.dataset.historyRange === range));
    });
    content.setAttribute('aria-busy', 'true');
    content.innerHTML = '<p class="empty" role="status">Loading uptime history…</p>';
    try {
        const history = await api(`/api/monitors/${encodeURIComponent(monitorId)}/history?range=${range}`);
        // A slower response must not replace a newer period or another monitor's history.
        if (version !== requestVersion) return;
        content.innerHTML = renderHistory(history);
        $$('[data-history-bucket]', content).forEach((button) => {
            const showDetail = () => {
                $('#history-bucket-detail').textContent = bucketDescription(history.buckets[Number(button.dataset.historyBucket)]);
            };
            button.addEventListener('focus', showDetail);
            button.addEventListener('click', showDetail);
        });
    } catch (error) {
        if (version !== requestVersion) return;
        content.innerHTML = `<p class="empty" role="alert">${esc(error.message)}</p><button type="button" class="btn-secondary" data-history-retry>Retry</button>`;
        $('[data-history-retry]', content).addEventListener('click', () => loadHistory(selectedRange));
    } finally {
        if (version === requestVersion) content.setAttribute('aria-busy', 'false');
    }
}

export function openHistory(id, name) {
    monitorId = id;
    $('#logs-title').textContent = `Uptime history · ${name || 'monitor'}`;
    openModal('#logs-modal');
    return loadHistory('24h');
}

export function initHistory() {
    $$('[data-history-range]').forEach((button) => {
        button.addEventListener('click', () => {
            if (monitorId) loadHistory(button.dataset.historyRange);
        });
    });
}
