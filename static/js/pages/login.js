// Login / register page. After signing in, users return to `?next=` (same-origin paths only)
// or land on the dashboard.

import { api } from '../api.js';
import { fetchCurrentUser, renderHeader } from '../auth.js';
import { $, $$, navigate } from '../ui.js';

function nextUrl() {
    const next = new URLSearchParams(location.search).get('next');
    return next && next.startsWith('/') && !next.startsWith('//') ? next : '/dashboard';
}

function currentTab() {
    return location.hash === '#register' ? 'register' : 'login';
}

function setTab(tab) {
    $$('[role="tab"]').forEach((button) => button.setAttribute('aria-selected', String(button.dataset.tab === tab)));
    $('#auth-form').dataset.mode = tab;
    $('#auth-submit').textContent = tab === 'register' ? 'Create account' : 'Login';
    $('#auth-title').textContent = tab === 'register' ? 'Create your account' : 'Welcome back';
    $('#auth-password').autocomplete = tab === 'register' ? 'new-password' : 'current-password';
    $('#auth-error').textContent = '';
}

async function submit(event) {
    event.preventDefault();
    const mode = $('#auth-form').dataset.mode || 'login';
    const email = $('#auth-email').value.trim();
    const password = $('#auth-password').value;
    const errorBox = $('#auth-error');
    errorBox.textContent = '';
    if (!email || password.length < 8) {
        errorBox.textContent = 'Enter your email and a password of at least 8 characters.';
        return;
    }
    const button = $('#auth-submit');
    button.disabled = true;
    try {
        await api(`/api/auth/${mode}`, { method: 'POST', body: { email, password } });
        navigate(nextUrl());
    } catch (error) {
        errorBox.textContent = error.message;
        button.disabled = false;
    }
}

async function main() {
    const user = await fetchCurrentUser();
    renderHeader(user);
    if (user) { navigate(nextUrl()); return; }
    setTab(currentTab());
    window.addEventListener('hashchange', () => setTab(currentTab()));
    $$('[role="tab"]').forEach((button) => button.addEventListener('click', () => {
        history.replaceState(null, '', button.dataset.tab === 'register' ? '#register' : '#login');
        setTab(button.dataset.tab);
    }));
    $('#auth-form').addEventListener('submit', submit);
    $('#auth-email').focus();
}

main();
