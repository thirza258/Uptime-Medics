//! Frontend delivery: the `static/` tree (HTML layout, pages, partials, CSS, JS, images) is
//! embedded into the binary in release builds; debug builds read it from disk on every request,
//! so HTML/CSS/JS edits show up without recompiling. Pages are composed from
//! `html/layout.html`, one fragment in `html/pages/` and `{{> name}}` partials.

use std::collections::HashMap;
use std::sync::OnceLock;

use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "static/"]
struct Assets;

/// `(route, page name, title)`. The page name selects `html/pages/<name>.html`,
/// `css/pages/<name>.css` and `js/pages/<name>.js`.
pub const PAGES: &[(&str, &str, &str)] = &[
    (
        "/",
        "landing",
        "Uptime Medics | Fast Minimalist Uptime Monitoring",
    ),
    ("/dashboard", "dashboard", "My Monitors | Uptime Medics"),
    ("/courses", "courses", "Uptime Courses | Uptime Medics"),
    ("/login", "login", "Sign in | Uptime Medics"),
];

/// Specific public root files served directly.
const PUBLIC_FILES: &[&str] = &["robots.txt", "sitemap.xml", "ads.txt"];

/// Only these directories are served as raw files; `html/` fragments are never exposed.
const PUBLIC_PREFIXES: &[&str] = &["css/", "js/", "img/"];
const MAX_PARTIAL_EXPANSIONS: usize = 200;

fn read_text(path: &str) -> Option<String> {
    Assets::get(path).map(|file| String::from_utf8_lossy(&file.data).into_owned())
}

/// Compose a page from the layout, its fragment and any `{{> partial}}` includes.
pub fn render_page(page: &str, title: &str) -> Option<String> {
    let layout = read_text("html/layout.html")?;
    let content = read_text(&format!("html/pages/{page}.html"))?;
    let mut html = layout
        .replace("{{content}}", &content)
        .replace("{{title}}", title)
        .replace("{{page}}", page);

    let mut expansions = 0;
    while let Some(start) = html.find("{{>") {
        let Some(len) = html[start..].find("}}") else {
            break;
        };
        let end = start + len + 2;
        let name = html[start + 3..end - 2].trim().to_string();
        let partial = read_text(&format!("html/partials/{name}.html"))
            .unwrap_or_else(|| format!("<!-- missing partial: {name} -->"));
        html.replace_range(start..end, &partial);
        expansions += 1;
        if expansions >= MAX_PARTIAL_EXPANSIONS {
            break;
        }
    }
    Some(html)
}

/// Serve a composed page. Release builds render every page once and cache it.
pub fn serve_page(page: &'static str, title: &'static str) -> Response {
    if cfg!(debug_assertions) {
        return match render_page(page, title) {
            Some(html) => page_response(html),
            None => not_found(),
        };
    }
    static CACHE: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        PAGES
            .iter()
            .filter_map(|&(_, name, title)| render_page(name, title).map(|html| (name, html)))
            .collect()
    });
    match cache.get(page) {
        Some(html) => page_response(html.clone()),
        None => not_found(),
    }
}

fn page_response(html: String) -> Response {
    ([(header::CACHE_CONTROL, "no-cache")], Html(html)).into_response()
}

/// Serve ads.txt for ad networks.
pub async fn serve_ads_txt() -> Response {
    match Assets::get("ads.txt") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (
                    header::CACHE_CONTROL,
                    if cfg!(debug_assertions) {
                        "no-cache"
                    } else {
                        "public, max-age=86400"
                    },
                ),
            ],
            file.data,
        )
            .into_response(),
        None => not_found(),
    }
}

/// Serve robots.txt for search engines.
pub async fn serve_robots() -> Response {
    match Assets::get("robots.txt") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (
                    header::CACHE_CONTROL,
                    if cfg!(debug_assertions) {
                        "no-cache"
                    } else {
                        "public, max-age=86400"
                    },
                ),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => not_found(),
    }
}

/// Serve sitemap.xml for search engines.
pub async fn serve_sitemap() -> Response {
    match Assets::get("sitemap.xml") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
                (
                    header::CACHE_CONTROL,
                    if cfg!(debug_assertions) {
                        "no-cache"
                    } else {
                        "public, max-age=86400"
                    },
                ),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        None => not_found(),
    }
}

/// Fallback handler: serve `css/`, `js/` and `img/` files, 404 for everything else.
pub async fn asset_or_404(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let allowed = PUBLIC_FILES.contains(&path)
        || PUBLIC_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix));
    if !allowed
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
    {
        return not_found();
    }
    match Assets::get(path) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            let content_type = if mime.starts_with("text/")
                || mime == "application/javascript"
                || mime == "application/xml"
            {
                format!("{mime}; charset=utf-8")
            } else {
                mime.to_string()
            };
            let cache_control = if cfg!(debug_assertions) {
                "no-cache"
            } else {
                "public, max-age=3600"
            };
            (
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => not_found(),
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "404 Not Found").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_render_without_leftover_tokens() {
        for &(_, page, title) in PAGES {
            let html = render_page(page, title).expect(page);
            assert!(html.contains(&format!("<title>{title}</title>")));
            assert!(html.contains(&format!("data-page=\"{page}\"")));
            assert!(html.contains(&format!("/js/pages/{page}.js")));
            assert!(
                html.contains("class=\"site-header\"") && html.contains("class=\"site-footer\"")
            );
            assert!(!html.contains("{{"), "{page}: unexpanded template token");
            assert!(!html.contains("missing partial"), "{page}: missing partial");
        }
        assert!(render_page("nope", "x").is_none());
    }

    #[test]
    fn every_page_has_its_stylesheet_and_script() {
        for &(_, page, _) in PAGES {
            assert!(
                Assets::get(&format!("css/pages/{page}.css")).is_some(),
                "{page}.css"
            );
            assert!(
                Assets::get(&format!("js/pages/{page}.js")).is_some(),
                "{page}.js"
            );
        }
    }

    #[test]
    fn static_seo_files_exist() {
        assert!(Assets::get("ads.txt").is_some(), "ads.txt must exist");
        assert!(Assets::get("robots.txt").is_some(), "robots.txt must exist");
        assert!(
            Assets::get("sitemap.xml").is_some(),
            "sitemap.xml must exist"
        );
    }
}
