// Landing page: system health tiles, the shared create form, remembered anonymous submissions
// and the "10 random juggle" community watch.

import { api } from '../api.js';
import { fetchCurrentUser, renderHeader } from '../auth.js';
import { initLogsInteractions, initMonitorForm } from '../monitors.js';
import { $, initModals, monitorRow, storage } from '../ui.js';

const JUGGLE_INTERVAL_MS = 10000;
const RECENT_KEY = 'um_recent_monitors';

let user = null;
let recent = (Array.isArray(storage.get(RECENT_KEY, [])) ? storage.get(RECENT_KEY, []) : [])
    .filter((id) => typeof id === 'string')
    .slice(0, 10);
let countdown = JUGGLE_INTERVAL_MS / 1000;

async function loadStats() {
    try {
        const stats = await api('/api/public/stats');
        $('#stat-public').textContent = stats.public_monitors.toLocaleString();
        $('#stat-up').textContent = stats.up.toLocaleString();
        $('#stat-down').textContent = stats.down.toLocaleString();
        $('#stat-checks').textContent = stats.total_checks.toLocaleString();
    } catch (error) {
        console.warn('stats unavailable', error);
    }
}

async function fetchRandomMonitors() {
    try {
        const data = await api('/api/public/random-monitors');
        const container = $('#juggle-container');
        container.innerHTML = data.length
            ? data.map(monitorRow).join('')
            : '<p class="empty">No public monitors active yet. Be the first – submit a URL above!</p>';
        countdown = JUGGLE_INTERVAL_MS / 1000;
    } catch (error) {
        console.error('Failed to juggle monitors', error);
    }
}

function tickCountdown() {
    const status = $('#refresh-status');
    if (document.hidden || !status) return;
    countdown = Math.max(0, countdown - 1);
    status.textContent = `10 random monitored targets · next juggle in ${countdown}s`;
}

function rememberRecent(id) {
    recent = [id, ...recent.filter((other) => other !== id)].slice(0, 10);
    storage.set(RECENT_KEY, recent);
}

async function renderRecent() {
    const section = $('#recent');
    if (user || !recent.length) { section.hidden = true; return; }
    section.hidden = false;
    const results = await Promise.all(recent.map(async (id) => {
        try { return await api(`/api/monitors/${encodeURIComponent(id)}`); } catch (error) {
            return error.status === 404 ? { missing: id } : null;
        }
    }));
    const missing = results.filter((r) => r && r.missing).map((r) => r.missing);
    if (missing.length) {
        recent = recent.filter((id) => !missing.includes(id));
        storage.set(RECENT_KEY, recent);
    }
    if (!recent.length) { section.hidden = true; return; }
    const monitors = results.filter((r) => r && !r.missing);
    $('#recent-container').innerHTML = monitors.length
        ? monitors.map(monitorRow).join('')
        : '<p class="empty">Could not load your submissions right now.</p>';
}

async function main() {
    initModals();
    initLogsInteractions();
    user = await fetchCurrentUser();
    renderHeader(user);
    initMonitorForm({
        isPrivate: Boolean(user),
        onCreated: (monitor) => {
            if (!user) { rememberRecent(monitor.id); renderRecent(); }
            fetchRandomMonitors();
            loadStats();
        },
    });
    $('#forget-recent').addEventListener('click', () => { recent = []; storage.set(RECENT_KEY, recent); renderRecent(); });

    await Promise.all([loadStats(), fetchRandomMonitors(), renderRecent()]);
    setInterval(() => { if (!document.hidden) { fetchRandomMonitors(); loadStats(); renderRecent(); } }, JUGGLE_INTERVAL_MS);
    setInterval(tickCountdown, 1000);
    document.addEventListener('visibilitychange', () => { if (!document.hidden) { fetchRandomMonitors(); loadStats(); } });
}

main();
