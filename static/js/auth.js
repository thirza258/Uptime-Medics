// Session helpers shared by every page: current user, header rendering, logout, auth guard.

import { api } from './api.js';
import { $, $$, esc, navigate } from './ui.js';

/** The signed-in user (`{ id, email }`) or null. */
export async function fetchCurrentUser() {
    try { return await api('/api/auth/me'); } catch (_) { return null; }
}

/** Render the header's auth area and highlight the current page in the navigation. */
export function renderHeader(user) {
    const page = document.body.dataset.page;
    $$('.nav [data-nav]').forEach((link) => link.classList.toggle('active', link.dataset.nav === page));
    const box = $('#auth-actions');
    if (!box) return;
    if (user) {
        box.innerHTML = `<span class="who" title="${esc(user.email)}">${esc(user.email)}</span>
            <button type="button" class="btn-secondary" id="logout-btn">Logout</button>`;
        $('#logout-btn').addEventListener('click', logout);
    } else {
        const next = page === 'landing' ? '' : `?next=${encodeURIComponent(location.pathname)}`;
        box.innerHTML = `<a class="btn-primary btn-small" href="/login${next}">Login</a>
            <a class="btn-secondary" href="/login${next}#register">Register</a>`;
    }
}

export async function logout() {
    try { await api('/api/auth/logout', { method: 'POST' }); } catch (_) { /* cookie is cleared regardless */ }
    navigate('/');
}

/** Redirect to the login page (remembering where we were) unless a user is signed in. */
export async function requireUser() {
    const user = await fetchCurrentUser();
    if (!user) navigate(`/login?next=${encodeURIComponent(location.pathname)}`);
    return user;
}
