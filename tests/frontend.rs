//! The multi-page frontend: page composition from layout + partials, static asset delivery,
//! and integrity of every referenced stylesheet, script and module import.

mod common;

use std::collections::{BTreeSet, VecDeque};

use common::TestApp;

/// Extract `attr="/..."` values (e.g. `src="/js/pages/landing.js"`) from HTML.
fn attribute_paths(html: &str, attr: &str) -> Vec<String> {
    let needle = format!("{attr}=\"/");
    html.match_indices(&needle)
        .filter_map(|(index, _)| {
            let rest = &html[index + needle.len() - 1..];
            rest.find('"').map(|end| rest[..end].to_string())
        })
        .collect()
}

/// Resolve `import ... from './x.js'` / `'../x.js'` specifiers relative to a module URL path.
fn module_imports(source: &str, module_path: &str) -> Vec<String> {
    let dir = &module_path[..module_path.rfind('/').unwrap_or(0)];
    let mut imports = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if !line.starts_with("import ") && !line.starts_with("export ") {
            continue;
        }
        let Some(from) = line.find(" from '") else {
            continue;
        };
        let spec = &line[from + 7..];
        let Some(end) = spec.find('\'') else { continue };
        let spec = &spec[..end];
        let mut segments: VecDeque<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        for part in spec.split('/') {
            match part {
                "." => {}
                ".." => {
                    segments.pop_back();
                }
                other => segments.push_back(other),
            }
        }
        imports.push(format!(
            "/{}",
            segments.iter().copied().collect::<Vec<_>>().join("/")
        ));
    }
    imports
}

#[tokio::test]
async fn pages_are_composed_from_layout_and_partials() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let pages = [
        (
            "/",
            "landing",
            "Uptime Medics | Fast Minimalist Uptime Monitoring",
            "id=\"juggle-container\"",
        ),
        (
            "/index.html",
            "landing",
            "Uptime Medics | Fast Minimalist Uptime Monitoring",
            "id=\"juggle-container\"",
        ),
        (
            "/dashboard",
            "dashboard",
            "My Monitors | Uptime Medics",
            "id=\"dashboard-body\"",
        ),
        (
            "/login",
            "login",
            "Sign in | Uptime Medics",
            "id=\"auth-form\"",
        ),
    ];
    for (path, page, title, marker) in pages {
        let res = client.get(path).await.assert_status(200);
        let html = &res.text;
        assert!(
            res.header("content-type").unwrap().starts_with("text/html"),
            "{path}"
        );
        assert!(html.starts_with("<!DOCTYPE html>"), "{path}");
        assert!(html.contains(&format!("<title>{title}</title>")), "{path}");
        assert!(
            html.contains(&format!("<body data-page=\"{page}\">")),
            "{path}"
        );
        assert!(html.contains(marker), "{path} should contain {marker}");
        assert!(
            html.contains("class=\"site-header\"") && html.contains("class=\"site-footer\""),
            "{path}: header/footer partials"
        );
        assert!(
            html.contains("id=\"auth-actions\""),
            "{path}: header auth area"
        );
        assert!(
            html.contains(&format!(
                "<script type=\"module\" src=\"/js/pages/{page}.js\"></script>"
            )),
            "{path}"
        );
        assert!(
            html.contains(&format!("href=\"/css/pages/{page}.css\"")),
            "{path}"
        );
        assert!(!html.contains("{{"), "{path}: unexpanded template token");
        assert!(!html.contains("missing partial"), "{path}: missing partial");
        assert!(
            !html.contains("<script>"),
            "{path}: no inline scripts (CSP forbids them)"
        );
        assert!(!html.contains("<style>"), "{path}: no inline stylesheets");
    }

    let landing = client.get("/").await.text;
    let dashboard = client.get("/dashboard").await.text;
    let login = client.get("/login").await.text;
    // The monitor form partial is shared by landing and dashboard, the edit modal is dashboard-only,
    // and the login page has neither.
    assert!(landing.contains("id=\"monitor-form\"") && dashboard.contains("id=\"monitor-form\""));
    assert!(!login.contains("id=\"monitor-form\""));
    assert!(landing.contains("id=\"logs-modal\"") && dashboard.contains("id=\"logs-modal\""));
    assert!(dashboard.contains("id=\"edit-modal\"") && !landing.contains("id=\"edit-modal\""));
    assert!(landing.contains("id=\"stat-public\"") && !dashboard.contains("id=\"stat-public\""));
    assert!(login.contains("role=\"tablist\"") && login.contains("id=\"tab-register\""));
    app.finish().await;
}

#[tokio::test]
async fn every_referenced_asset_and_module_import_resolves() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let mut pending: VecDeque<String> = VecDeque::new();
    for path in ["/", "/dashboard", "/login"] {
        let html = client.get(path).await.assert_status(200).text;
        for asset in attribute_paths(&html, "src")
            .into_iter()
            .chain(attribute_paths(&html, "href"))
        {
            if asset.starts_with("/js/") || asset.starts_with("/css/") || asset.starts_with("/img/")
            {
                pending.push_back(asset);
            }
        }
    }
    assert!(pending.iter().any(|p| p == "/img/favicon.svg"));
    assert!(pending.iter().any(|p| p == "/css/base.css"));

    let mut checked: BTreeSet<String> = BTreeSet::new();
    while let Some(asset) = pending.pop_front() {
        if !checked.insert(asset.clone()) {
            continue;
        }
        let res = client.get(&asset).await;
        assert_eq!(res.status, 200, "{asset} is referenced but missing");
        let content_type = res.header("content-type").unwrap_or_default();
        if asset.ends_with(".js") {
            assert!(
                content_type.contains("javascript"),
                "{asset}: {content_type}"
            );
            for import in module_imports(&res.text, &asset) {
                pending.push_back(import);
            }
        } else if asset.ends_with(".css") {
            assert!(
                content_type.starts_with("text/css"),
                "{asset}: {content_type}"
            );
        } else if asset.ends_with(".svg") {
            assert!(
                content_type.starts_with("image/svg+xml"),
                "{asset}: {content_type}"
            );
        }
    }
    for required in [
        "/js/api.js",
        "/js/ui.js",
        "/js/auth.js",
        "/js/monitors.js",
        "/js/pages/landing.js",
        "/js/pages/dashboard.js",
        "/js/pages/login.js",
        "/css/components.css",
    ] {
        assert!(
            checked.contains(required),
            "{required} should be reachable from a page; checked: {checked:?}"
        );
    }
    // The landing script talks to the juggle endpoint; the login script to the auth endpoints.
    assert!(client
        .get("/js/pages/landing.js")
        .await
        .text
        .contains("/api/public/random-monitors"));
    assert!(client
        .get("/js/pages/login.js")
        .await
        .text
        .contains("/api/auth/"));
    app.finish().await;
}

#[tokio::test]
async fn static_files_have_types_and_unknown_paths_are_404() {
    let app = TestApp::spawn().await;
    let client = app.client();

    let css = client.get("/css/base.css").await.assert_status(200);
    assert!(css.header("content-type").unwrap().starts_with("text/css"));
    assert!(css.header("cache-control").is_some());
    assert!(css.text.contains("--mcd-yellow: #FFC72C"));

    let js = client.get("/js/api.js").await.assert_status(200);
    assert!(js.header("content-type").unwrap().contains("javascript"));
    assert!(js.text.contains("export async function api"));

    let svg = client.get("/img/favicon.svg").await.assert_status(200);
    assert!(svg
        .header("content-type")
        .unwrap()
        .starts_with("image/svg+xml"));

    for missing in [
        "/js/missing.js",
        "/css/pages/nope.css",
        "/html/layout.html",
        "/html/partials/header.html",
        "/html/pages/landing.html",
        "/Cargo.toml",
        "/static/js/api.js",
        "/js/",
        "/nope",
    ] {
        let res = client.get(missing).await;
        assert_eq!(res.status, 404, "{missing} must not be served");
    }
    app.finish().await;
}
