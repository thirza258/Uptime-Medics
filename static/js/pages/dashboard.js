// Dashboard: a signed-in user's private monitors with pause/edit/delete, plus the create form.

import { api } from '../api.js';
import { renderHeader, requireUser } from '../auth.js';
import { initLogsInteractions, initMonitorForm } from '../monitors.js';
import { $, $$, closeModals, esc, formatInterval, initModals, methodBadge, navigate, parseHeadersField, statusBadge, toast, truncate } from '../ui.js';

const REFRESH_MS = 15000;
let monitors = [];
let editing = null;

function renderTable() {
    const body = $('#dashboard-body');
    $('#dashboard-empty').hidden = monitors.length > 0;
    $('#dashboard-table').hidden = monitors.length === 0;
    $('#dashboard-hint').textContent = `${monitors.length} monitor${monitors.length === 1 ? '' : 's'} · private to your account`;
    body.innerHTML = monitors.map((m) => {
        const latency = m.response_time_ms != null ? `${m.response_time_ms} ms` : '—';
        const error = m.current_status === 'DOWN' && m.last_error
            ? `<span class="mono fail-text cell-error" title="${esc(m.last_error)}">${esc(truncate(m.last_error, 70))}</span>`
            : '';
        return `<tr data-id="${esc(m.id)}">
            <td>${statusBadge(m)}</td>
            <td class="cell-target"><div class="monitor-name">${esc(m.name)}</div><div class="url-truncate" title="${esc(m.url)}">${esc(truncate(m.url, 60))}</div>${error}</td>
            <td>${methodBadge(m.method)}</td>
            <td class="num">${esc(latency)}</td>
            <td>${esc(formatInterval(m.check_interval_seconds))}</td>
            <td class="muted">${m.alert_email ? esc(truncate(m.alert_email, 24)) : '—'}</td>
            <td><label class="switch" title="${m.is_active ? 'Pause' : 'Resume'} monitoring"><input type="checkbox" data-toggle="${esc(m.id)}" ${m.is_active ? 'checked' : ''} aria-label="Active"><span class="slider"></span></label></td>
            <td><div class="row-actions">
                <button type="button" class="btn-secondary" data-logs="${esc(m.id)}" data-name="${esc(m.name)}">History</button>
                <button type="button" class="btn-secondary" data-edit="${esc(m.id)}">Edit</button>
                <button type="button" class="btn-danger" data-delete="${esc(m.id)}" data-name="${esc(m.name)}">Delete</button>
            </div></td>
        </tr>`;
    }).join('');
}

async function loadDashboard() {
    try {
        monitors = await api('/api/monitors/user');
    } catch (error) {
        if (error.status === 401) navigate('/login?next=%2Fdashboard');
        return;
    }
    renderTable();
}

async function toggleMonitor(id, active) {
    try {
        await api(`/api/monitors/${encodeURIComponent(id)}`, { method: 'PATCH', body: { is_active: active } });
        toast(active ? 'Monitoring resumed' : 'Monitoring paused', 'info');
    } catch (error) { toast(error.message, 'error'); }
    loadDashboard();
}

async function deleteMonitor(id, name) {
    if (!confirm(`Delete monitor "${name}" and all of its history?`)) return;
    try {
        await api(`/api/monitors/${encodeURIComponent(id)}`, { method: 'DELETE' });
        toast('Monitor deleted', 'success');
    } catch (error) { toast(error.message, 'error'); }
    loadDashboard();
}

function openEdit(id) {
    const monitor = monitors.find((m) => m.id === id);
    if (!monitor) return;
    editing = id;
    $('#edit-url').value = monitor.url;
    $('#edit-method').value = monitor.method;
    $('#edit-name').value = monitor.name;
    $('#edit-email').value = monitor.alert_email || '';
    $('#edit-expected').value = monitor.expected_status_code;
    const intervalSelect = $('#edit-interval');
    if (![...intervalSelect.options].some((o) => Number(o.value) === monitor.check_interval_seconds)) {
        const option = document.createElement('option');
        option.value = monitor.check_interval_seconds;
        option.textContent = `${monitor.check_interval_seconds} seconds`;
        intervalSelect.appendChild(option);
    }
    intervalSelect.value = String(monitor.check_interval_seconds);
    $('#edit-timeout').value = monitor.timeout_seconds;
    let headersText = '';
    if (monitor.headers) {
        try { headersText = JSON.stringify(JSON.parse(monitor.headers), null, 2); } catch (_) { headersText = monitor.headers; }
    }
    $('#edit-headers').value = headersText;
    $('#edit-body').value = monitor.body || '';
    $('#edit-error').textContent = '';
    $('#edit-modal').hidden = false;
    $('#edit-url').focus();
}

async function submitEdit(event) {
    event.preventDefault();
    if (!editing) return;
    const errorBox = $('#edit-error');
    errorBox.textContent = '';
    let headers;
    try { headers = parseHeadersField($('#edit-headers').value); } catch (error) { errorBox.textContent = error.message; return; }
    const body = {
        url: $('#edit-url').value.trim(),
        method: $('#edit-method').value,
        name: $('#edit-name').value.trim(),
        alert_email: $('#edit-email').value.trim() || null,
        expected_status_code: Number($('#edit-expected').value) || 200,
        check_interval_seconds: Number($('#edit-interval').value) || 60,
        timeout_seconds: Number($('#edit-timeout').value) || 10,
        headers,
        body: $('#edit-body').value || null,
    };
    try {
        await api(`/api/monitors/${encodeURIComponent(editing)}`, { method: 'PATCH', body });
        closeModals();
        toast('Monitor updated', 'success');
        loadDashboard();
    } catch (error) { errorBox.textContent = error.message; }
}

async function main() {
    initModals();
    initLogsInteractions();
    const user = await requireUser();
    if (!user) return;
    renderHeader(user);
    initMonitorForm({ isPrivate: true, onCreated: loadDashboard });

    document.addEventListener('click', (event) => {
        const target = event.target.closest('[data-edit], [data-delete]');
        if (!target) return;
        if (target.dataset.edit) openEdit(target.dataset.edit);
        else deleteMonitor(target.dataset.delete, target.dataset.name);
    });
    document.addEventListener('change', (event) => {
        const toggle = event.target.closest('input[data-toggle]');
        if (toggle) toggleMonitor(toggle.dataset.toggle, toggle.checked);
    });
    $('#edit-form').addEventListener('submit', submitEdit);

    await loadDashboard();
    setInterval(() => { if (!document.hidden) loadDashboard(); }, REFRESH_MS);
    document.addEventListener('visibilitychange', () => { if (!document.hidden) loadDashboard(); });
    void $$;
}

main();
