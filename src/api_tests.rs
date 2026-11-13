
use crate::app_state::ApplicationState;
use crate::engine::worker_tests::{Mock, SPY, USDC_BASE, USDC_SOL, seed_assets, seed_user, state};
use reqwest::{Client, StatusCode};
use std::sync::atomic::Ordering;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct App {
    base: String,
    http: Client,
    state: ApplicationState,
    mock: Mock,
}

impl App {
    async fn start(pool: &PgPool) -> Self {
        Self::start_with(pool, Mock::new("complete")).await
    }

    async fn start_with(pool: &PgPool, mock: Mock) -> Self {
        Self::start_configured(pool, mock, |_| {}).await
    }

    async fn start_configured(pool: &PgPool, mock: Mock, configure: impl FnOnce(&mut crate::config::AppConfig)) -> Self {
        let mut state = state(pool, &mock, true).await;
        configure(&mut state.config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = crate::routes::app_router(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { base, http: Client::new(), state, mock }
    }

    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = self.http.request(method.parse().unwrap(), format!("{}{path}", self.base));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call("POST", path, None, Some(body)).await
    }

    async fn register(&self, email: &str) -> Value {
        let (status, body) = self.post("/auth/register", json!({ "email": email, "password": "correct horse battery" })).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["data"].clone()
    }
}

fn token(tokens: &Value, field: &str) -> String {
    tokens[field].as_str().unwrap().to_string()
}

#[sqlx::test]
async fn full_email_session_lifecycle(pool: PgPool) {
    let app = App::start(&pool).await;
    let tokens = app.register("Ada@Example.com").await;
    assert_eq!(tokens["token_type"], "Bearer");

    let (status, _) = app.post("/auth/register", json!({ "email": "ada@example.com", "password": "another long password" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "email is case-insensitive");
    let (status, _) = app.post("/auth/register", json!({ "email": "bob@example.com", "password": "short" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = app.post("/auth/login", json!({ "email": "ada@example.com", "password": "wrong password!!" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app.post("/auth/login", json!({ "email": "nobody@example.com", "password": "correct horse battery" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = app.post("/auth/login", json!({ "email": "ada@example.com", "password": "correct horse battery" })).await;
    assert_eq!(status, StatusCode::OK);
    let login = body["data"].clone();

    let (status, me) = app.call("GET", "/auth/me", Some(&token(&login, "access_token")), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["data"]["email"], "ada@example.com");
    assert_eq!(me["data"]["telegram_linked"], false);
    let (status, _) = app.call("GET", "/auth/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app.call("GET", "/auth/me", Some("not-a-token"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Refresh rotates the token; replaying the old one burns the whole session.
    let old_refresh = token(&login, "refresh_token");
    let (status, body) = app.post("/auth/refresh", json!({ "refresh_token": old_refresh })).await;
    assert_eq!(status, StatusCode::OK);
    let new_refresh = token(&body["data"], "refresh_token");
    assert_ne!(new_refresh, old_refresh);
    let (status, _) = app.post("/auth/refresh", json!({ "refresh_token": old_refresh })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app.post("/auth/refresh", json!({ "refresh_token": new_refresh })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the reuse revoked the family");

    let (_, body) = app.post("/auth/login", json!({ "email": "ada@example.com", "password": "correct horse battery" })).await;
    let (a, b) = (token(&body["data"], "refresh_token"), token(&tokens, "refresh_token"));
    assert_eq!(app.post("/auth/logout", json!({ "refresh_token": a })).await.0, StatusCode::OK);
    assert_eq!(app.post("/auth/refresh", json!({ "refresh_token": a })).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(app.post("/auth/refresh", json!({ "refresh_token": b })).await.0, StatusCode::OK);

    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_refresh_tokens WHERE token_hash = $1").bind(&b).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, 0, "refresh tokens are stored hashed, never raw");
}

#[sqlx::test]
async fn repeated_bad_passwords_are_throttled(pool: PgPool) {
    let app = App::start(&pool).await;
    app.register("ada@example.com").await;
    for _ in 0..10 {
        let (status, _) = app.post("/auth/login", json!({ "email": "ada@example.com", "password": "wrong password!!" })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = app.post("/auth/login", json!({ "email": "ada@example.com", "password": "correct horse battery" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) = app.post("/auth/login", json!({ "email": "other@example.com", "password": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "other accounts are unaffected");
}

#[sqlx::test]
async fn telegram_codes_sign_in_and_link(pool: PgPool) {
    let app = App::start(&pool).await;
    let telegram_id = 4242i64;
    sqlx::query("INSERT INTO users (telegram_id, username, gateway_user_id) VALUES ($1, 'tg', 'gw-tg')").bind(telegram_id).execute(&pool).await.unwrap();

    let code = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    let (status, body) = app.post("/auth/telegram/login", json!({ "code": code.to_lowercase() })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = app.post("/auth/telegram/login", json!({ "code": code })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "codes work once");
    let (status, _) = app.post("/auth/telegram/login", json!({ "code": "ZZZZZZZZ" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let first = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    let second = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    assert_eq!(app.post("/auth/telegram/login", json!({ "code": first })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(app.post("/auth/telegram/login", json!({ "code": second })).await.0, StatusCode::OK);

    let stale = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    sqlx::query("UPDATE telegram_login_codes SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    assert_eq!(app.post("/auth/telegram/login", json!({ "code": stale })).await.0, StatusCode::BAD_REQUEST);

    // Linking: an email user takes a Telegram account that has no user yet...
    let tokens = app.register("ada@example.com").await;
    let access = token(&tokens, "access_token");
    let free_id = 9001i64;
    let code = crate::services::auth::issue_telegram_code(&app.state, free_id, None).await.unwrap();
    let (status, body) = app.call("POST", "/auth/telegram/link", Some(&access), Some(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, me) = app.call("GET", "/auth/me", Some(&access), None).await;
    assert_eq!(me["data"]["telegram_linked"], true);

    // ...but never merges into an existing account.
    let other = app.register("bob@example.com").await;
    let code = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    let (status, _) = app.call("POST", "/auth/telegram/link", Some(&token(&other, "access_token")), Some(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let code = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
    let (_, body) = app.post("/auth/telegram/login", json!({ "code": code })).await;
    let tg_access = token(&body["data"], "access_token");
    let creds = json!({ "email": "tg@example.com", "password": "correct horse battery" });
    assert_eq!(app.call("POST", "/auth/credentials", Some(&tg_access), Some(creds.clone())).await.0, StatusCode::OK);
    assert_eq!(app.call("POST", "/auth/credentials", Some(&tg_access), Some(creds)).await.0, StatusCode::CONFLICT);
    assert_eq!(app.post("/auth/login", json!({ "email": "tg@example.com", "password": "correct horse battery" })).await.0, StatusCode::OK);
}

#[sqlx::test]
async fn mobile_routes_need_a_session_and_a_finished_account(pool: PgPool) {
    let app = App::start(&pool).await;
    for path in ["/v1/portfolio", "/v1/deposit", "/v1/rule", "/v1/orders", "/v1/investments", "/v1/assets", "/v1/onboarding"] {
        assert_eq!(app.call("GET", path, None, None).await.0, StatusCode::UNAUTHORIZED, "{path}");
    }
    let tokens = app.register("ada@example.com").await;
    let access = token(&tokens, "access_token");

    let (_, body) = app.call("GET", "/v1/onboarding", Some(&access), None).await;
    assert_eq!(body["data"]["stage"], "needs_rule");
    for path in ["/v1/portfolio", "/v1/deposit"] {
        assert_eq!(app.call("GET", path, Some(&access), None).await.0, StatusCode::CONFLICT, "{path}");
    }

    let (status, _) = app.call("PUT", "/v1/rule", Some(&access), Some(json!({ "percentage": 15, "plan": "steady" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "only the offered percentages are accepted");
    let (status, _) = app.call("PUT", "/v1/rule", Some(&access), Some(json!({ "percentage": 10, "plan": "custom" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "custom needs picks first");
    let (status, body) = app.call("PUT", "/v1/rule", Some(&access), Some(json!({ "percentage": 10, "plan": "steady" }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = app.call("GET", "/v1/rule", Some(&access), None).await;
    assert_eq!((body["data"]["percentage"].as_i64(), body["data"]["enabled"].as_bool()), (Some(10), Some(false)));
    let (_, body) = app.call("GET", "/v1/onboarding", Some(&access), None).await;
    assert_eq!(body["data"]["stage"], "needs_gateway");
}

#[sqlx::test]
async fn manual_buys_are_validated_and_idempotent(pool: PgPool) {
    seed_assets(&pool).await;
    let app = App::start(&pool).await;
    let tokens = app.register("ada@example.com").await;
    let access = token(&tokens, "access_token");
    sqlx::query("UPDATE users SET gateway_user_id = 'gw-1', verified = 'verified' WHERE email = 'ada@example.com'")
        .execute(&pool)
        .await
        .unwrap();

    let buy = |asset: &str, amount: &str, reference: Option<&str>| {
        json!({ "asset_key": asset, "amount_usd": amount, "client_ref": reference })
    };
    let post = |body: Value| app.call("POST", "/v1/trades", Some(&access), Some(body));

    assert_eq!(post(buy("spy-id", "10", None)).await.0, StatusCode::BAD_REQUEST, "client_ref required");
    assert_eq!(post(buy("spy-id", "0.5", Some("a"))).await.0, StatusCode::BAD_REQUEST, "below the floor");
    assert_eq!(post(buy("spy-id", "51", Some("a"))).await.0, StatusCode::BAD_REQUEST, "above the order limit");
    assert_eq!(post(buy("spy-id", "ten", Some("a"))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(post(buy("nope", "10", Some("a"))).await.0, StatusCode::BAD_REQUEST, "unknown asset");

    let (status, first) = post(buy("spy-id", "10", Some("ref-1"))).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (_, again) = post(buy("spy-id", "10", Some("ref-1"))).await;
    assert_eq!(first["data"]["id"], again["data"]["id"], "the same client_ref is the same order");
    assert_eq!(post(buy("spy-id", "20", Some("ref-1"))).await.0, StatusCode::CONFLICT, "ref reuse with a different order");
    assert_eq!(post(buy("qqq-id", "10", Some("ref-1"))).await.0, StatusCode::CONFLICT);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);

    let id = first["data"]["id"].as_str().unwrap();
    assert_eq!(app.call("GET", &format!("/v1/orders/{id}"), Some(&access), None).await.0, StatusCode::OK);
    let other = token(&app.register("bob@example.com").await, "access_token");
    assert_eq!(app.call("GET", &format!("/v1/orders/{id}"), Some(&other), None).await.0, StatusCode::NOT_FOUND);
    let (_, list) = app.call("GET", "/v1/orders", Some(&other), None).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn investments_list_shows_legs_and_hides_operator_states(pool: PgPool) {
    seed_assets(&pool).await;
    let app = App::start(&pool).await;
    let user = seed_user(&pool).await;
    let email_tokens = crate::services::auth::access_token_for_tests(&app.state, user).await;
    crate::engine::worker_tests::seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    crate::engine::worker::plan_investments(&pool, &app.state.config.engine).await.unwrap();
    sqlx::query("UPDATE orders SET status = 'needs_review' WHERE asset_key = 'qqq-id'").execute(&pool).await.unwrap();

    let (status, body) = app.call("GET", "/v1/investments", Some(&email_tokens), None).await;
    assert_eq!(status, StatusCode::OK);
    let item = &body["data"][0];
    assert_eq!(item["invested_usd"], "10.00");
    let statuses: Vec<&str> = item["orders"].as_array().unwrap().iter().map(|o| o["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, vec!["in_progress", "in_progress"]);
    assert!(!body.to_string().contains("needs_review"));
}

#[sqlx::test]
async fn admin_api_is_locked_and_validates_mixes(pool: PgPool) {
    seed_assets(&pool).await;
    let app = App::start(&pool).await;
    let admin = |method: &'static str, path: String, key: Option<&'static str>, body: Option<Value>| {
        let mut req = app.http.request(method.parse().unwrap(), format!("{}{path}", app.base));
        if let Some(key) = key {
            req = req.header("x-admin-key", key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move { req.send().await.unwrap().status() }
    };

    assert_eq!(admin("GET", "/admin/orders/review".into(), None, None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(admin("GET", "/admin/orders/review".into(), Some("wrong"), None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(admin("GET", "/admin/orders/review".into(), Some("test-admin-key-1234"), None).await, StatusCode::OK);

    let key = Some("test-admin-key-1234");
    let ok = json!([{ "asset_key": "spy-id", "weight_bps": 7000 }, { "asset_key": "qqq-id", "weight_bps": 3000 }]);
    let short = json!([{ "asset_key": "spy-id", "weight_bps": 7000 }]);
    let unknown = json!([{ "asset_key": "ghost", "weight_bps": 10000 }]);
    assert_eq!(admin("PUT", "/admin/plans/growth/allocations".into(), key, Some(short)).await, StatusCode::BAD_REQUEST);
    assert_eq!(admin("PUT", "/admin/plans/growth/allocations".into(), key, Some(unknown)).await, StatusCode::BAD_REQUEST);
    assert_eq!(admin("PUT", "/admin/plans/custom/allocations".into(), key, Some(ok.clone())).await, StatusCode::BAD_REQUEST);
    assert_eq!(admin("PUT", "/admin/plans/growth/allocations".into(), key, Some(ok)).await, StatusCode::OK);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plan_allocations WHERE plan = 'growth'").fetch_one(&pool).await.unwrap();
    assert_eq!(rows, 2);

    let user = seed_user(&pool).await;
    let order: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (user_id, kind, asset_key, amount_micros, status) VALUES ($1, 'manual', 'spy-id', 5000000, 'needs_review') RETURNING id",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    let path = format!("/admin/orders/{order}/resolve");
    assert_eq!(admin("POST", path.clone(), key, Some(json!({ "status": "retry", "note": "x" }))).await, StatusCode::BAD_REQUEST);
    assert_eq!(admin("POST", path.clone(), key, Some(json!({ "status": "failed", "note": " " }))).await, StatusCode::BAD_REQUEST);
    assert_eq!(admin("POST", path.clone(), key, Some(json!({ "status": "failed", "note": "checked: never executed" }))).await, StatusCode::OK);
    assert_eq!(admin("POST", path, key, Some(json!({ "status": "completed", "note": "again" }))).await, StatusCode::NOT_FOUND);
}



const PASSWORD: &str = "correct horse battery";
const SOL_ADDRESS: &str = "So11111111111111111111111111111111111111112";
const EVM_ADDRESS: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";

async fn seed_money_assets(pool: &PgPool) {
    sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'SPY', 'SPY', 'base', true, false) ON CONFLICT (asset_key) DO NOTHING")
        .bind(SPY)
        .execute(pool)
        .await
        .unwrap();
    for (key, chain) in [(USDC_SOL, "solana"), (USDC_BASE, "base")] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'USDC', 'USDC', $2, true, true) ON CONFLICT (asset_key) DO NOTHING")
            .bind(key)
            .bind(chain)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn active_user(app: &App, pool: &PgPool, email: &str) -> (String, Uuid) {
    let tokens = app.register(email).await;
    sqlx::query("UPDATE users SET gateway_user_id = $2, verified = 'verified' WHERE email = $1")
        .bind(email)
        .bind(format!("gw-{email}"))
        .execute(pool)
        .await
        .unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1").bind(email).fetch_one(pool).await.unwrap();
    (token(&tokens, "access_token"), id)
}

#[sqlx::test]
async fn selling_is_checked_sized_in_the_stock_and_idempotent(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, _) = active_user(&app, &pool, "ada@example.com").await;
    let sell = |body: Value| app.call("POST", "/v1/sells", Some(&access), Some(body));

    let (status, holdings) = app.call("GET", "/v1/holdings", Some(&access), None).await;
    assert_eq!(status, StatusCode::OK, "{holdings}");
    let spy = &holdings["data"][0];
    assert_eq!((spy["symbol"].as_str(), spy["available"].as_str(), spy["value_usd"].as_str()), (Some("SPY"), Some("2"), Some("1000.00")));
    assert_eq!(holdings["data"].as_array().unwrap().len(), 1, "cash is not a holding");

    let bad = |qty: &str, reference: &str| json!({ "asset_key": SPY, "qty": qty, "client_ref": reference });
    assert_eq!(sell(json!({ "asset_key": SPY, "qty": "0.5" })).await.0, StatusCode::BAD_REQUEST, "client_ref required");
    assert_eq!(sell(bad("3", "r")).await.0, StatusCode::BAD_REQUEST, "more than held");
    assert_eq!(sell(bad("0", "r")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(sell(bad("abc", "r")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(sell(bad("0.001", "r")).await.0, StatusCode::BAD_REQUEST, "worth 0.50 USD, under the floor");
    assert_eq!(sell(bad("2", "r")).await.0, StatusCode::BAD_REQUEST, "1000 USD is over the single-sale limit");
    assert_eq!(sell(json!({ "asset_key": USDC_SOL, "qty": "1", "client_ref": "r" })).await.0, StatusCode::BAD_REQUEST, "cash is not sold");
    assert_eq!(sell(json!({ "asset_key": SPY, "client_ref": "r" })).await.0, StatusCode::BAD_REQUEST, "needs qty or all");

    let (status, first) = sell(bad("0.5", "sell-1")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["side"], "sell");
    assert_eq!(first["data"]["amount_usd"], "250.00");
    let (_, again) = sell(bad("0.5", "sell-1")).await;
    assert_eq!(first["data"]["id"], again["data"]["id"]);
    assert_eq!(sell(bad("0.6", "sell-1")).await.0, StatusCode::CONFLICT, "same ref, different sale");
    let rows: Vec<(String, Option<String>)> = sqlx::query_as("SELECT side, qty_base FROM orders").fetch_all(&pool).await.unwrap();
    assert_eq!(rows, vec![("sell".to_string(), Some("0.5".to_string()))]);

    *app.mock.spy_available.lock().unwrap() = "0.8".into();
    let (status, all) = sell(json!({ "asset_key": SPY, "all": true, "client_ref": "all-1" })).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["data"]["amount_usd"], "400.00");

    let (status, quote) = app.call("POST", "/v1/sells/quote", Some(&access), Some(json!({ "asset_key": SPY, "qty": "0.5" }))).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["data"]["estimated_usd"], "250.00");
}

#[sqlx::test]
async fn the_daily_selling_limit_holds(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, _) = active_user(&app, &pool, "ada@example.com").await;
    *app.mock.spy_available.lock().unwrap() = "0.8".into(); // 400 USD each
    for n in 0..5 {
        let (status, body) = app.call("POST", "/v1/sells", Some(&access), Some(json!({ "asset_key": SPY, "all": true, "client_ref": format!("s{n}") }))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _) = app.call("POST", "/v1/sells", Some(&access), Some(json!({ "asset_key": SPY, "all": true, "client_ref": "s5" }))).await;
    assert_eq!(status, StatusCode::CONFLICT, "2000 USD a day");
}

#[sqlx::test]
async fn saving_a_withdrawal_address_needs_the_password_and_a_good_address(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, user) = active_user(&app, &pool, "ada@example.com").await;
    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, $2, 'solana')")
        .bind(user)
        .bind("9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin")
        .execute(&pool)
        .await
        .unwrap();
    let add = |address: &str, password: &str| {
        app.call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": address, "password": password, "label": "Ledger" })))
    };

    assert_eq!(add(SOL_ADDRESS, "wrong password!!").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(add("not-an-address", PASSWORD).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(add("0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAEd", PASSWORD).await.0, StatusCode::BAD_REQUEST, "bad EIP-55 checksum");
    assert_eq!(add("0x0000000000000000000000000000000000000000", PASSWORD).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(add("9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", PASSWORD).await.0, StatusCode::BAD_REQUEST, "their own deposit address");

    let (status, saved) = add(SOL_ADDRESS, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!((saved["data"]["usable"].as_bool(), saved["data"]["chain_family"].as_str()), (Some(false), Some("solana")));
    assert_eq!(add(SOL_ADDRESS, PASSWORD).await.0, StatusCode::CONFLICT);
    let (status, evm) = add(EVM_ADDRESS, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(evm["data"]["address"], EVM_ADDRESS.to_ascii_lowercase());

    let (_, list) = app.call("GET", "/v1/withdraw/addresses", Some(&access), None).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 2);
    let id = saved["data"]["id"].as_str().unwrap();
    assert_eq!(app.call("DELETE", &format!("/v1/withdraw/addresses/{id}"), Some(&access), None).await.0, StatusCode::OK);
    assert_eq!(app.call("DELETE", &format!("/v1/withdraw/addresses/{id}"), Some(&access), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.call("GET", "/v1/withdraw/addresses", Some(&access), None).await.1["data"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn withdrawals_are_gated_by_cooldown_password_network_and_limits(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, user) = active_user(&app, &pool, "ada@example.com").await;
    let (_, saved) = app
        .call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": SOL_ADDRESS, "password": PASSWORD })))
        .await;
    let address_id = saved["data"]["id"].as_str().unwrap().to_string();
    let withdraw = |amount: &str, asset: &str, password: &str, reference: &str| {
        app.call(
            "POST",
            "/v1/withdrawals",
            Some(&access),
            Some(json!({ "address_id": address_id, "asset_key": asset, "amount_usd": amount, "password": password, "client_ref": reference })),
        )
    };

    let (status, body) = withdraw("10", USDC_SOL, PASSWORD, "w0").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["message"].as_str().unwrap().contains("can be used from"));
    sqlx::query("UPDATE withdrawal_addresses SET usable_at = now() - interval '1 minute'").execute(&pool).await.unwrap();

    assert_eq!(withdraw("10", USDC_SOL, "wrong password!!", "w1").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(withdraw("10", USDC_BASE, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST, "Base asset, Solana address");
    assert_eq!(withdraw("10", SPY, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST, "stocks are sold, not withdrawn");
    assert_eq!(withdraw("0.5", USDC_SOL, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST, "under the minimum");
    assert_eq!(withdraw("101", USDC_SOL, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST, "over the per-withdrawal limit");
    assert_eq!(withdraw("ten", USDC_SOL, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST);
    *app.mock.usdc_available.lock().unwrap() = "5".into();
    assert_eq!(withdraw("10", USDC_SOL, PASSWORD, "w1").await.0, StatusCode::BAD_REQUEST, "not enough available");
    *app.mock.usdc_available.lock().unwrap() = "500".into();

    let (status, first) = withdraw("100", USDC_SOL, PASSWORD, "w1").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!((first["data"]["status"].as_str(), first["data"]["to"].as_str()), (Some("in_progress"), Some(SOL_ADDRESS)));
    let (_, again) = withdraw("100", USDC_SOL, PASSWORD, "w1").await;
    assert_eq!(first["data"]["id"], again["data"]["id"], "the same client_ref is the same withdrawal");
    assert_eq!(withdraw("50", USDC_SOL, PASSWORD, "w1").await.0, StatusCode::CONFLICT, "ref reuse for a different withdrawal");
    assert_eq!(withdraw("100", USDC_SOL, PASSWORD, "w2").await.0, StatusCode::OK);
    assert_eq!(withdraw("1", USDC_SOL, PASSWORD, "w3").await.0, StatusCode::CONFLICT, "200 USD a day");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM withdrawals").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2);

    let id = first["data"]["id"].as_str().unwrap();
    assert_eq!(app.call("GET", &format!("/v1/withdrawals/{id}"), Some(&access), None).await.0, StatusCode::OK);
    let (other, _) = active_user(&app, &pool, "bob@example.com").await;
    assert_eq!(app.call("GET", &format!("/v1/withdrawals/{id}"), Some(&other), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(app.call("GET", "/v1/withdrawals", Some(&other), None).await.1["data"].as_array().unwrap().len(), 0);

    sqlx::query("UPDATE withdrawal_addresses SET removed_at = now()").execute(&pool).await.unwrap();
    assert_eq!(withdraw("10", USDC_SOL, PASSWORD, "w4").await.0, StatusCode::NOT_FOUND);
    let _ = user;
}

#[sqlx::test]
async fn guessing_the_password_to_withdraw_is_throttled(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, _) = active_user(&app, &pool, "ada@example.com").await;
    for _ in 0..5 {
        let (status, _) = app
            .call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": SOL_ADDRESS, "password": "wrong password!!" })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = app
        .call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": SOL_ADDRESS, "password": PASSWORD })))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "even the right password waits once the guesses run out");
}


// ---- Telegram Mini App and linking ----

fn launch_data(app: &App, id: i64, age_secs: i64) -> String {
    let user = json!({ "id": id, "username": format!("user{id}") });
    crate::services::auth::sign_init_data(&app.state.config.telegram_bot_token, chrono::Utc::now().timestamp() - age_secs, &user)
}

#[sqlx::test]
async fn the_mini_app_signs_a_telegram_user_in_without_a_code(pool: PgPool) {
    let app = App::start(&pool).await;
    let (status, body) = app.post("/auth/telegram/webapp", json!({ "init_data": launch_data(&app, 555, 5) })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let access = token(&body["data"], "access_token");

    let (_, me) = app.call("GET", "/auth/me", Some(&access), None).await;
    assert_eq!((me["data"]["telegram_linked"].as_bool(), me["data"]["email"].is_null()), (Some(true), true));
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE telegram_id = 555").fetch_one(&pool).await.unwrap();
    assert_eq!(username.as_deref(), Some("user555"));

    // Opening it again is the same account, and the session works everywhere else.
    app.post("/auth/telegram/webapp", json!({ "init_data": launch_data(&app, 555, 5) })).await;
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&pool).await.unwrap();
    assert_eq!(users, 1);
    let (_, stage) = app.call("GET", "/v1/onboarding", Some(&access), None).await;
    assert_eq!(stage["data"]["stage"], "needs_rule");
}

#[sqlx::test]
async fn forged_stale_or_foreign_launch_data_is_refused(pool: PgPool) {
    let app = App::start(&pool).await;
    let good = launch_data(&app, 555, 5);
    let attempts = [
        good.replace("555", "556"),
        launch_data(&app, 555, 7200),
        crate::services::auth::sign_init_data("some-other-bot", chrono::Utc::now().timestamp(), &json!({ "id": 555 })),
        good.split('&').filter(|p| !p.starts_with("hash=")).collect::<Vec<_>>().join("&"),
        "garbage".to_string(),
        String::new(),
    ];
    for init_data in attempts {
        let (status, _) = app.post("/auth/telegram/webapp", json!({ "init_data": init_data })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{init_data}");
    }
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&pool).await.unwrap();
    assert_eq!(users, 0, "a refused launch never creates an account");

    for _ in 0..60 {
        app.post("/auth/telegram/webapp", json!({ "init_data": "garbage" })).await;
    }
    let (status, _) = app.post("/auth/telegram/webapp", json!({ "init_data": good })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test]
async fn a_code_signs_in_a_telegram_user_who_never_opened_the_bot_flow(pool: PgPool) {
    let app = App::start(&pool).await;
    let code = crate::services::auth::issue_telegram_code(&app.state, 777, Some("fresh")).await.unwrap();
    let (status, _) = app.post("/auth/telegram/login", json!({ "code": code })).await;
    assert_eq!(status, StatusCode::OK);
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE telegram_id = 777").fetch_one(&pool).await.unwrap();
    assert_eq!(username.as_deref(), Some("fresh"));
}

#[sqlx::test]
async fn an_email_account_links_to_a_telegram_account_that_only_said_start(pool: PgPool) {
    let app = App::start(&pool).await;
    let access = token(&app.register("ada@example.com").await, "access_token");

    // They tapped /start in the bot and picked a tenth, but never got further.
    let early = crate::services::onboarding::ensure_user(&app.state, 888, Some("ada_tg")).await.unwrap();
    crate::services::onboarding::save_rule(&app.state, early.user_id, 20, &crate::models::schemas::InvestmentPlan::Growth).await.unwrap();

    let code = crate::services::auth::issue_telegram_code(&app.state, 888, Some("ada_tg")).await.unwrap();
    let (status, body) = app.call("POST", "/auth/telegram/link", Some(&access), Some(json!({ "code": code }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&pool).await.unwrap();
    assert_eq!(users, 1, "the empty Telegram-side account was absorbed");
    let (percentage, linked, username): (i16, i64, Option<String>) = sqlx::query_as(
        "SELECT r.percentage, u.telegram_id, u.username FROM users u JOIN rules r ON r.user_id = u.id WHERE u.email = 'ada@example.com'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((percentage, linked, username.as_deref()), (20, 888, Some("ada_tg")), "their tenth choice came along");

    // The Telegram side now opens the same account.
    let (_, tg) = app.post("/auth/telegram/webapp", json!({ "init_data": launch_data(&app, 888, 1) })).await;
    let (_, me) = app.call("GET", "/auth/me", Some(&token(&tg["data"], "access_token")), None).await;
    assert_eq!(me["data"]["email"], "ada@example.com");
}

#[sqlx::test]
async fn linking_keeps_the_email_accounts_own_tenth(pool: PgPool) {
    let app = App::start(&pool).await;
    let tokens = app.register("ada@example.com").await;
    let access = token(&tokens, "access_token");
    let (_, me) = app.call("GET", "/auth/me", Some(&access), None).await;
    let mine: Uuid = me["data"]["id"].as_str().unwrap().parse().unwrap();
    crate::services::onboarding::save_rule(&app.state, mine, 5, &crate::models::schemas::InvestmentPlan::Steady).await.unwrap();
    let early = crate::services::onboarding::ensure_user(&app.state, 889, None).await.unwrap();
    crate::services::onboarding::save_rule(&app.state, early.user_id, 20, &crate::models::schemas::InvestmentPlan::Tech).await.unwrap();

    let code = crate::services::auth::issue_telegram_code(&app.state, 889, None).await.unwrap();
    assert_eq!(app.call("POST", "/auth/telegram/link", Some(&access), Some(json!({ "code": code }))).await.0, StatusCode::OK);
    let rules: Vec<i16> = sqlx::query_scalar("SELECT percentage FROM rules").fetch_all(&pool).await.unwrap();
    assert_eq!(rules, vec![5]);
}

#[sqlx::test]
async fn linking_never_absorbs_a_telegram_account_with_real_state(pool: PgPool) {
    let app = App::start(&pool).await;
    let access = token(&app.register("ada@example.com").await, "access_token");

    let with_gateway = crate::services::onboarding::ensure_user(&app.state, 890, None).await.unwrap();
    sqlx::query("UPDATE users SET gateway_user_id = 'gw-890' WHERE id = $1").bind(with_gateway.user_id).execute(&pool).await.unwrap();
    let with_wallet = crate::services::onboarding::ensure_user(&app.state, 891, None).await.unwrap();
    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, 'addr', 'solana')").bind(with_wallet.user_id).execute(&pool).await.unwrap();

    for telegram_id in [890, 891] {
        let code = crate::services::auth::issue_telegram_code(&app.state, telegram_id, None).await.unwrap();
        let (status, _) = app.call("POST", "/auth/telegram/link", Some(&access), Some(json!({ "code": code }))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{telegram_id}");
    }
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&pool).await.unwrap();
    assert_eq!(users, 3, "nothing was deleted");
    let linked: Option<i64> = sqlx::query_scalar("SELECT telegram_id FROM users WHERE email = 'ada@example.com'").fetch_one(&pool).await.unwrap();
    assert_eq!(linked, None);
}


// ---- Confirming irreversible actions ----

fn last_code(mock: &Mock) -> String {
    let messages = mock.telegram.lock().unwrap();
    let text = messages
        .iter()
        .rev()
        .filter_map(|m| m["text"].as_str())
        .find(|t| t.contains("confirmation code"))
        .expect("no code was sent")
        .to_string();
    let end = text.rfind("</code>").unwrap();
    let start = text[..end].rfind("<code>").unwrap() + "<code>".len();
    text[start..end].to_string()
}

fn code_messages(mock: &Mock) -> usize {
    mock.telegram.lock().unwrap().iter().filter(|m| m["text"].as_str().is_some_and(|t| t.contains("confirmation code"))).count()
}

async fn telegram_only(app: &App, pool: &PgPool) -> (String, Uuid) {
    seed_money_assets(pool).await;
    let user = seed_user(pool).await;
    (crate::services::auth::access_token_for_tests(&app.state, user).await, user)
}

async fn saved_address(pool: &PgPool, user: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO withdrawal_addresses (user_id, chain_family, address, usable_at) VALUES ($1, 'solana', $2, now() - interval '1 minute') RETURNING id",
    )
    .bind(user)
    .bind(SOL_ADDRESS)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn the_confirmation_method_follows_how_the_account_is_secured(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let ask = |access: String| {
        let app = &app;
        async move { app.call("POST", "/v1/confirm", Some(&access), Some(json!({ "action": "add_address", "address": SOL_ADDRESS }))).await }
    };

    // Email and password: the password confirms, and nothing is sent anywhere.
    let (email_access, _) = active_user(&app, &pool, "ada@example.com").await;
    let (status, body) = ask(email_access).await;
    assert_eq!((status, body["data"]["method"].as_str()), (StatusCode::OK, Some("password")));
    assert_eq!(code_messages(&app.mock), 0);

    // Telegram only: a code goes to their chat, naming the address it confirms.
    let (tg_access, _) = telegram_only(&app, &pool).await;
    let (status, body) = ask(tg_access).await;
    assert_eq!((status, body["data"]["method"].as_str(), body["data"]["expires_in"].as_i64()), (StatusCode::OK, Some("telegram_code"), Some(300)));
    let sent = app.mock.telegram.lock().unwrap().last().unwrap().clone();
    assert!(sent["text"].as_str().unwrap().contains(SOL_ADDRESS));
    assert_eq!(last_code(&app.mock).len(), 6);

    // Both: the stronger factor wins, so a Telegram code is never offered.
    let (both_access, both) = active_user(&app, &pool, "bob@example.com").await;
    sqlx::query("UPDATE users SET telegram_id = 4321 WHERE id = $1").bind(both).execute(&pool).await.unwrap();
    let before = code_messages(&app.mock);
    let (_, body) = ask(both_access).await;
    assert_eq!(body["data"]["method"], "password");
    assert_eq!(code_messages(&app.mock), before);

    // Neither: there is nothing to confirm with.
    let nobody: Uuid = sqlx::query_scalar("INSERT INTO users (gateway_user_id, verified) VALUES ('gw-x', 'verified') RETURNING id").fetch_one(&pool).await.unwrap();
    let access = crate::services::auth::access_token_for_tests(&app.state, nobody).await;
    assert_eq!(ask(access).await.0, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn a_telegram_only_user_adds_an_address_with_a_code_for_exactly_that_address(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, _) = telegram_only(&app, &pool).await;
    let add = |address: &str, code: Option<&str>| {
        app.call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": address, "code": code })))
    };
    assert_eq!(add(SOL_ADDRESS, None).await.0, StatusCode::BAD_REQUEST, "a code is required");

    app.call("POST", "/v1/confirm", Some(&access), Some(json!({ "action": "add_address", "address": SOL_ADDRESS }))).await;
    let code = last_code(&app.mock);
    assert_eq!(add(SOL_ADDRESS, Some("WRONG1")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(add(EVM_ADDRESS, Some(&code)).await.0, StatusCode::BAD_REQUEST, "the code is for a different address");

    let (status, saved) = add(SOL_ADDRESS, Some(&code.to_lowercase())).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(add(SOL_ADDRESS, Some(&code)).await.0, StatusCode::BAD_REQUEST, "a code works once");
}

#[sqlx::test]
async fn wrong_tries_burn_a_code_and_old_codes_expire(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, _) = telegram_only(&app, &pool).await;
    let challenge = || app.call("POST", "/v1/confirm", Some(&access), Some(json!({ "action": "add_address", "address": SOL_ADDRESS })));
    let add = |code: &str| app.call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": SOL_ADDRESS, "code": code })));

    challenge().await;
    let burned = last_code(&app.mock);
    for _ in 0..5 {
        assert_eq!(add("WRONG1").await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(add(&burned).await.0, StatusCode::BAD_REQUEST, "five wrong tries spend the code, even the right one");

    challenge().await;
    let stale = last_code(&app.mock);
    sqlx::query("UPDATE action_confirmations SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    assert_eq!(add(&stale).await.0, StatusCode::BAD_REQUEST, "expired");

    challenge().await;
    let first = last_code(&app.mock);
    challenge().await;
    assert_eq!(add(&first).await.0, StatusCode::BAD_REQUEST, "a newer challenge replaces the older one");
}

#[sqlx::test]
async fn a_telegram_only_user_withdraws_with_a_code_bound_to_that_withdrawal(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = telegram_only(&app, &pool).await;
    let address_id = saved_address(&pool, user).await;
    *app.mock.usdc_available.lock().unwrap() = "500".into();
    let request = |amount: &str, reference: &str, code: Option<&str>| {
        json!({ "address_id": address_id, "asset_key": USDC_SOL, "amount_usd": amount, "client_ref": reference, "code": code })
    };
    let withdraw = |body: Value| app.call("POST", "/v1/withdrawals", Some(&access), Some(body));

    assert_eq!(withdraw(request("10", "w1", None)).await.0, StatusCode::BAD_REQUEST, "no code, no withdrawal");

    let challenge = json!({ "action": "withdraw", "address_id": address_id, "asset_key": USDC_SOL, "amount_usd": "10", "client_ref": "w1" });
    assert_eq!(app.call("POST", "/v1/confirm", Some(&access), Some(challenge)).await.0, StatusCode::OK);
    let sent = app.mock.telegram.lock().unwrap().last().unwrap()["text"].as_str().unwrap().to_string();
    assert!(sent.contains("$10.00") && sent.contains(SOL_ADDRESS), "{sent}");
    let code = last_code(&app.mock);

    assert_eq!(withdraw(request("20", "w1", Some(&code))).await.0, StatusCode::BAD_REQUEST, "a bigger amount needs its own code");
    assert_eq!(withdraw(request("10", "w2", Some(&code))).await.0, StatusCode::BAD_REQUEST, "so does another request");
    let (status, first) = withdraw(request("10", "w1", Some(&code))).await;
    assert_eq!(status, StatusCode::OK, "{first}");

    // Repeating the same request returns the original; nothing new is created or confirmed.
    let (status, again) = withdraw(request("10", "w1", None)).await;
    assert_eq!((status, &again["data"]["id"]), (StatusCode::OK, &first["data"]["id"]));
    assert_eq!(withdraw(request("20", "w1", None)).await.0, StatusCode::CONFLICT, "the ref belongs to the first request");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM withdrawals").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn a_user_with_a_password_cannot_swap_in_a_code(pool: PgPool) {
    seed_money_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, user) = active_user(&app, &pool, "ada@example.com").await;
    sqlx::query("UPDATE users SET telegram_id = 4321 WHERE id = $1").bind(user).execute(&pool).await.unwrap();
    app.call("POST", "/v1/confirm", Some(&access), Some(json!({ "action": "add_address", "address": SOL_ADDRESS }))).await;
    let (status, body) = app.call("POST", "/v1/withdraw/addresses", Some(&access), Some(json!({ "address": SOL_ADDRESS, "code": "ABC234" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["message"].as_str().unwrap().contains("password"));
}

#[sqlx::test]
async fn code_requests_are_throttled_and_fail_closed_when_telegram_is_down(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, _) = telegram_only(&app, &pool).await;
    let challenge = || app.call("POST", "/v1/confirm", Some(&access), Some(json!({ "action": "add_address", "address": SOL_ADDRESS })));

    app.mock.telegram_down.store(true, Ordering::SeqCst);
    assert_eq!(challenge().await.0, StatusCode::CONFLICT);
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM action_confirmations WHERE used_at IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(live, 0, "a code that never reached the user is not left usable");

    app.mock.telegram_down.store(false, Ordering::SeqCst);
    for _ in 0..4 {
        assert_eq!(challenge().await.0, StatusCode::OK);
    }
    assert_eq!(challenge().await.0, StatusCode::TOO_MANY_REQUESTS, "five a quarter-hour");
}


// ---- Identity checks ----

const NIN: &str = "12345678901";

/// A signed-in user who has finished everything before the identity step.
async fn ready_for_kyc(app: &App, pool: &PgPool, email: &str) -> (String, Uuid) {
    let access = token(&app.register(email).await, "access_token");
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1").bind(email).fetch_one(pool).await.unwrap();
    crate::services::onboarding::save_rule(&app.state, id, 10, &crate::models::schemas::InvestmentPlan::Steady).await.unwrap();
    sqlx::query("UPDATE users SET gateway_user_id = $2 WHERE id = $1").bind(id).bind(format!("gw-{email}")).execute(pool).await.unwrap();
    (access, id)
}

fn submit(app: &App, access: &str, name: &str, nin: &str) -> impl std::future::Future<Output = (StatusCode, Value)> {
    let call = app.call("POST", "/v1/kyc", Some(access), Some(json!({ "full_name": name, "nin": nin })));
    async move { call.await }
}

async fn record(pool: &PgPool, user: Uuid) -> (String, Option<String>, bool) {
    sqlx::query_as("SELECT status, reason, nin_encrypted IS NOT NULL FROM kyc_session_records WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_matching_name_verifies_the_user_and_switches_the_tenth_on(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    sqlx::query("UPDATE users SET telegram_id = 4321 WHERE id = $1").bind(user).execute(&pool).await.unwrap();

    let (_, before) = app.call("GET", "/v1/kyc", Some(&access), None).await;
    assert_eq!(before["data"]["status"], "none");

    let (status, body) = submit(&app, &access, "Obi Adaeze", NIN).await;
    assert_eq!((status, body["data"]["status"].as_str()), (StatusCode::OK, Some("verified")), "{body}");
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 1);

    let (verified, enabled): (bool, bool) = sqlx::query_as("SELECT u.verified = 'verified', r.enabled FROM users u JOIN rules r ON r.user_id = u.id WHERE u.id = $1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(verified && enabled);
    assert_eq!(record(&pool, user).await, ("verified".to_string(), None, false), "the encrypted NIN is wiped once decided");
    let (_, stage) = app.call("GET", "/v1/onboarding", Some(&access), None).await;
    assert_eq!(stage["data"]["stage"], "active");

    for _ in 0..40 {
        if app.mock.telegram.lock().unwrap().iter().any(|m| m["text"].as_str().is_some_and(|t| t.contains("You are verified"))) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the user was never told");
}

#[sqlx::test]
async fn the_nin_is_never_stored_in_the_clear(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, _) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    *app.mock.prembly.lock().unwrap() = "outage".into();
    submit(&app, &access, "Obi Adaeze", NIN).await; // stays pending with the encrypted NIN held for the retry
    let everything: String = sqlx::query_scalar("SELECT row_to_json(k)::text FROM kyc_session_records k").fetch_one(&pool).await.unwrap();
    assert!(!everything.contains(NIN), "{everything}");
    assert!(everything.contains("•••••••8901"), "only the last four digits are kept for display");
}

#[sqlx::test]
async fn a_wrong_name_fails_and_can_be_corrected(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;

    let (_, body) = submit(&app, &access, "Musa Bello", NIN).await;
    assert_eq!((body["data"]["status"].as_str(), body["data"]["reason"].as_str(), body["data"]["can_retry"].as_bool()), (Some("failed"), Some("name_mismatch"), Some(true)));
    assert_eq!(record(&pool, user).await.2, false);
    let verified: String = sqlx::query_scalar("SELECT verified::text FROM users WHERE id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(verified, "unverified");

    let (_, again) = submit(&app, &access, "Adaeze Obi", NIN).await;
    assert_eq!(again["data"]["status"], "verified");
}

#[sqlx::test]
async fn a_partial_match_waits_for_a_human_who_can_approve_or_reject(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    let (_, body) = submit(&app, &access, "Adaeze Eze", NIN).await;
    assert_eq!((body["data"]["status"].as_str(), body["data"]["can_retry"].as_bool()), (Some("review"), Some(false)));
    assert_eq!(record(&pool, user).await.2, true, "kept, so the decision never bills a second lookup");
    let user_state: String = sqlx::query_scalar("SELECT verified::text FROM users WHERE id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(user_state, "review");

    // Asking again while it is under review changes nothing and calls nobody.
    submit(&app, &access, "Adaeze Eze", NIN).await;
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 1);

    let admin = |method: &'static str, path: String, body: Option<Value>| {
        let mut req = app.http.request(method.parse().unwrap(), format!("{}{path}", app.base)).header("x-admin-key", "test-admin-key-1234");
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move {
            let response = req.send().await.unwrap();
            (response.status(), response.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (status, list) = admin("GET", "/admin/kyc/review".into(), None).await;
    assert_eq!(status, StatusCode::OK);
    let item = &list["data"][0];
    assert_eq!((item["nin_masked"].as_str(), item["full_name"].as_str()), (Some("•••••••8901"), Some("Adaeze Eze")));
    assert!(!list.to_string().contains(NIN));

    let id = item["id"].as_str().unwrap().to_string();
    assert_eq!(admin("POST", format!("/admin/kyc/{id}/resolve"), Some(json!({ "verified": true }))).await.0, StatusCode::OK);
    assert_eq!(record(&pool, user).await, ("verified".to_string(), None, false));
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM rules WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert!(enabled);
    assert_eq!(admin("POST", format!("/admin/kyc/{id}/resolve"), Some(json!({ "verified": false }))).await.0, StatusCode::NOT_FOUND, "decided once");

    // And a rejection.
    let (access2, user2) = ready_for_kyc(&app, &pool, "bob@example.com").await;
    submit(&app, &access2, "Adaeze Eze", "10987654321").await;
    let (_, list) = admin("GET", "/admin/kyc/review".into(), None).await;
    let id2 = list["data"][0]["id"].as_str().unwrap().to_string();
    admin("POST", format!("/admin/kyc/{id2}/resolve"), Some(json!({ "verified": false }))).await;
    assert_eq!(record(&pool, user2).await, ("failed".to_string(), Some("rejected_in_review".to_string()), false));
}

#[sqlx::test]
async fn an_unknown_nin_fails_without_retrying(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    *app.mock.prembly.lock().unwrap() = "not_found".into();
    let (_, body) = submit(&app, &access, "Adaeze Obi", NIN).await;
    assert_eq!((body["data"]["status"].as_str(), body["data"]["reason"].as_str()), (Some("failed"), Some("nin_not_found")));
    assert_eq!(record(&pool, user).await.2, false);
    let billed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kyc_lookups").fetch_one(&pool).await.unwrap();
    assert_eq!(billed, 1, "a definitive answer counts against the daily limit");
}

#[sqlx::test]
async fn vendor_outages_retry_with_backoff_then_go_to_review_without_using_the_budget(pool: PgPool) {
    let app = App::start(&pool).await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    *app.mock.prembly.lock().unwrap() = "account".into(); // our own billing problem, not the user's fault

    let (_, body) = submit(&app, &access, "Adaeze Obi", NIN).await;
    assert_eq!(body["data"]["status"], "pending");
    let (attempts, future): (i32, bool) = sqlx::query_as("SELECT attempts, next_attempt_at > now() + interval '20 seconds' FROM kyc_session_records").fetch_one(&pool).await.unwrap();
    assert_eq!((attempts, future), (1, true), "backed off");

    // Not due yet: nothing happens.
    crate::services::kyc::process_due(&app.state).await.unwrap();
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 1);

    for _ in 0..2 {
        sqlx::query("UPDATE kyc_session_records SET next_attempt_at = now()").execute(&pool).await.unwrap();
        crate::services::kyc::process_due(&app.state).await.unwrap();
    }
    assert_eq!(record(&pool, user).await.0, "review");
    assert_eq!(record(&pool, user).await.1.as_deref(), Some("vendor_outage"));
    let billed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kyc_lookups").fetch_one(&pool).await.unwrap();
    assert_eq!(billed, 0, "outages never use up the daily budget");
}

#[sqlx::test]
async fn a_second_vendor_takes_over_when_the_first_is_down(pool: PgPool) {
    let app = App::start_configured(&pool, Mock::new("complete"), |config| {
        let prembly = config.kyc.vendors[0].clone();
        let crate::integrations::kyc_vendor::VendorConfig::Prembly { base_url, .. } = &prembly else { unreachable!() };
        config.kyc.vendors = vec![
            prembly.clone(),
            crate::integrations::kyc_vendor::VendorConfig::Dojah { base_url: base_url.clone(), app_id: "app".into(), secret_key: "secret".into(), nin_path: None },
        ];
    })
    .await;
    let (access, user) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    *app.mock.prembly.lock().unwrap() = "outage".into();

    let (_, body) = submit(&app, &access, "Adaeze Obi", NIN).await;
    assert_eq!(body["data"]["status"], "verified", "{body}");
    assert_eq!((app.mock.prembly_calls.load(Ordering::SeqCst), app.mock.dojah_calls.load(Ordering::SeqCst)), (1, 1));
    let vendor: Option<String> = sqlx::query_scalar("SELECT vendor FROM kyc_session_records WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(vendor.as_deref(), Some("dojah"));

    // A definitive answer from the first vendor stops the search.
    let (access2, _) = ready_for_kyc(&app, &pool, "bob@example.com").await;
    *app.mock.prembly.lock().unwrap() = "not_found".into();
    let before = app.mock.dojah_calls.load(Ordering::SeqCst);
    submit(&app, &access2, "Adaeze Obi", "10987654321").await;
    assert_eq!(app.mock.dojah_calls.load(Ordering::SeqCst), before);
}

#[sqlx::test]
async fn bad_input_wrong_stage_and_daily_limits_are_enforced(pool: PgPool) {
    let app = App::start(&pool).await;
    let early = token(&app.register("early@example.com").await, "access_token");
    assert_eq!(submit(&app, &early, "Adaeze Obi", NIN).await.0, StatusCode::CONFLICT, "no tenth or account yet");

    let (access, _) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    for (name, nin) in [("Adaeze Obi", "123"), ("Adaeze Obi", "1234567890a"), ("Adaeze", NIN), ("A B", NIN), ("", NIN)] {
        assert_eq!(submit(&app, &access, name, nin).await.0, StatusCode::BAD_REQUEST, "{name} / {nin}");
    }
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 0, "nothing invalid reaches the paid vendor");

    // Three wrong-name answers use the day's budget, whatever NIN is typed.
    for nin in ["11111111111", "22222222222", "33333333333"] {
        assert_eq!(submit(&app, &access, "Musa Bello", nin).await.1["data"]["status"], "failed");
    }
    let (status, body) = submit(&app, &access, "Adaeze Obi", NIN).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 3);
}

#[sqlx::test]
async fn one_nin_cannot_verify_two_accounts_and_blocked_nins_never_do(pool: PgPool) {
    let app = App::start(&pool).await;
    let (first, _) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    assert_eq!(submit(&app, &first, "Adaeze Obi", NIN).await.1["data"]["status"], "verified");

    let (second, _) = ready_for_kyc(&app, &pool, "bob@example.com").await;
    let (status, body) = submit(&app, &second, "Adaeze Obi", NIN).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 1);

    let hash = crate::services::auth::sha256_hex(&format!("{}{}", "55555555555", app.state.config.kyc.nin_hash_salt));
    sqlx::query("INSERT INTO kyc_blocklist (nin_hash, reason) VALUES ($1, 'fraud')").bind(hash).execute(&pool).await.unwrap();
    let (_, body) = submit(&app, &second, "Adaeze Obi", "55555555555").await;
    assert_eq!((body["data"]["status"].as_str(), body["data"]["reason"].as_str()), (Some("failed"), Some("blocked")));
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 1, "a blocked NIN is never even looked up");
}

#[sqlx::test]
async fn auto_mode_exists_for_development_only(pool: PgPool) {
    let app = App::start_configured(&pool, Mock::new("complete"), |config| config.kyc.mode = crate::config::KycMode::Auto).await;
    let (access, _) = ready_for_kyc(&app, &pool, "ada@example.com").await;
    let (_, body) = submit(&app, &access, "Anyone At All", "00000000000").await;
    assert_eq!(body["data"]["status"], "verified");
    assert_eq!(app.mock.prembly_calls.load(Ordering::SeqCst), 0);
}


#[sqlx::test]
async fn the_preset_mixes_and_custom_picks_can_be_read(pool: PgPool) {
    seed_assets(&pool).await;
    let app = App::start(&pool).await;
    let (access, _) = active_user(&app, &pool, "ada@example.com").await;

    let (status, plans) = app.call("GET", "/v1/plans", Some(&access), None).await;
    assert_eq!(status, StatusCode::OK, "{plans}");
    let plans = plans["data"].as_array().unwrap();
    assert_eq!(plans.iter().map(|p| p["plan"].as_str().unwrap()).collect::<Vec<_>>(), vec!["steady", "growth", "tech"]);
    let steady = &plans[0];
    assert_eq!((steady["name"].as_str(), steady["stocks"].as_array().unwrap().len()), (Some("Steady"), 2));
    assert_eq!((steady["stocks"][0]["symbol"].as_str(), steady["stocks"][0]["weight_bps"].as_i64()), (Some("SPY"), Some(6000)));
    assert_eq!(plans[1]["stocks"].as_array().unwrap().len(), 0, "a mix nobody has configured is empty, not missing");

    let (_, none) = app.call("GET", "/v1/rule/custom", Some(&access), None).await;
    assert_eq!(none["data"].as_array().unwrap().len(), 0);
    let picks = json!([{ "asset_key": "spy-id", "weight_bps": 7000 }, { "asset_key": "qqq-id", "weight_bps": 3000 }]);
    assert_eq!(app.call("PUT", "/v1/rule/custom", Some(&access), Some(picks)).await.0, StatusCode::OK);
    let (_, mine) = app.call("GET", "/v1/rule/custom", Some(&access), None).await;
    assert_eq!((mine["data"][0]["symbol"].as_str(), mine["data"][0]["weight_bps"].as_i64()), (Some("SPY"), Some(7000)));
}
