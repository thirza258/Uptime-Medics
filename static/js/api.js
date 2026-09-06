// Thin fetch wrapper for the Uptime Medics JSON API.

export class ApiError extends Error {
    constructor(message, status, data) {
        super(message);
        this.name = 'ApiError';
        this.status = status;
        this.data = data;
    }
}

/**
 * Call an API endpoint. Throws `ApiError` (with `.status` and `.data`) for non-2xx responses.
 * @param {string} path e.g. "/api/monitors"
 * @param {{ method?: string, body?: unknown }} [options]
 */
export async function api(path, { method = 'GET', body } = {}) {
    const options = { method, credentials: 'same-origin', headers: {} };
    if (body !== undefined) {
        options.headers['Content-Type'] = 'application/json';
        options.body = JSON.stringify(body);
    }
    const response = await fetch(path, options);
    const text = await response.text();
    let data = null;
    if (text) {
        try { data = JSON.parse(text); } catch (_) { data = null; }
    }
    if (!response.ok) {
        throw new ApiError((data && data.message) || `Request failed (${response.status})`, response.status, data);
    }
    return data;
}
