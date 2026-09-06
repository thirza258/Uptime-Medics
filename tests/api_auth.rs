//! Account registration, login, logout and session-cookie behaviour over real HTTP.

mod common;

use common::TestApp;
use serde_json::json;
use uptime_medics::config::Config;
use uptime_medics::security::auth;

#[tokio::test]
async fn register_login_logout_flow() {
    let app = TestApp::spawn().await;
    let client = app.client();

    let res = client
        .register("Ada@Example.com", "correct horse battery")
        .await
        .assert_status(201);
    let body = res.json();
    assert_eq!(
        body["email"], "ada@example.com",
        "emails are normalised to lower case"
    );
    assert_eq!(body["id"].as_str().map(str::len), Some(36), "id is a UUID");
    let cookie = res
        .header("set-cookie")
        .expect("session cookie is set on register");
    assert!(cookie.starts_with("um_session="));
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax") && cookie.contains("Path=/")
    );
    assert!(
        !cookie.contains("Secure"),
        "Secure flag stays off unless configured"
    );

    let me = client.get("/api/auth/me").await.assert_status(200);
    assert_eq!(me.json()["email"], "ada@example.com");

    client
        .register("ada@example.com", "another password")
        .await
        .assert_status(409);

    let old_cookie = client.cookie().expect("client keeps the session cookie");
    let out = client
        .post_empty("/api/auth/logout")
        .await
        .assert_status(204);
    assert!(
        out.header("set-cookie").unwrap().contains("Max-Age=0"),
        "logout clears the cookie"
    );
    client.get("/api/auth/me").await.assert_status(401);

    // Replaying the old cookie must fail: the session row was deleted server-side.
    client.set_cookie(Some(old_cookie));
    client.get("/api/auth/me").await.assert_status(401);
    client.set_cookie(None);

    client
        .login("ada@example.com", "wrong password")
        .await
        .assert_status(401);
    client
        .login("unknown@example.com", "correct horse battery")
        .await
        .assert_status(401);
    let login = client
        .login("ADA@example.com", "correct horse battery")
        .await
        .assert_status(200);
    assert_eq!(login.json()["email"], "ada@example.com");
    client.get("/api/auth/me").await.assert_status(200);

    app.finish().await;
}

#[tokio::test]
async fn register_validates_input() {
    let app = TestApp::spawn().await;
    let client = app.client();

    let short = client
        .register("bob@example.com", "short")
        .await
        .assert_status(400);
    assert_eq!(short.json()["error"], "bad_request");
    assert!(short.json()["message"]
        .as_str()
        .unwrap()
        .contains("at least 8"));

    let bad_email = client
        .register("not-an-email", "long enough password")
        .await
        .assert_status(400);
    assert!(bad_email.json()["message"]
        .as_str()
        .unwrap()
        .contains("valid email"));

    let malformed = client
        .post_raw("/api/auth/register", "{not json")
        .await
        .assert_status(400);
    assert!(malformed.json()["message"]
        .as_str()
        .unwrap()
        .starts_with("Invalid JSON body"));

    client
        .post_json("/api/auth/register", json!({ "email": "bob@example.com" }))
        .await
        .assert_status(400);

    client.get("/api/auth/me").await.assert_status(401);
    app.finish().await;
}

#[tokio::test]
async fn secure_cookie_flag_follows_configuration() {
    let app = TestApp::spawn_with(|c| c.cookie_secure = true).await;
    let client = app.client();
    let res = client
        .register("secure@example.com", "correct horse battery")
        .await
        .assert_status(201);
    assert!(res.header("set-cookie").unwrap().contains("; Secure"));
    let out = client
        .post_empty("/api/auth/logout")
        .await
        .assert_status(204);
    assert!(out.header("set-cookie").unwrap().contains("; Secure"));
    app.finish().await;
}

#[tokio::test]
async fn forged_or_garbage_tokens_are_rejected() {
    let app = TestApp::spawn().await;
    let client = app.client();
    client
        .register("carol@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let user_id = client.get("/api/auth/me").await.assert_status(200).json()["id"]
        .as_str()
        .unwrap()
        .to_string();

    client.set_cookie(Some("um_session=garbage.token.value".to_string()));
    client.get("/api/auth/me").await.assert_status(401);

    // Signed with a different secret: rejected even though it names a real user.
    let other = Config {
        jwt_secret: "a-completely-different-secret-value-0123456789".to_string(),
        ..Config::default()
    };
    let forged = auth::issue_token(&other, &user_id, "some-session").unwrap();
    client.set_cookie(Some(format!("um_session={forged}")));
    client.get("/api/auth/me").await.assert_status(401);

    // Validly signed but pointing at a session that does not exist: rejected too.
    let orphan = auth::issue_token(&app.state.config, &user_id, "no-such-session").unwrap();
    client.set_cookie(Some(format!("um_session={orphan}")));
    client.get("/api/auth/me").await.assert_status(401);

    app.finish().await;
}
