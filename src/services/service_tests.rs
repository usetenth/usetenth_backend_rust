use crate::app_state::ApplicationState;
use crate::config::KycMode;
use crate::engine::worker_tests::{Mock, SPY, seed_user, state};
use crate::models::schemas::{InvestmentPlan, VerificationStatus};
use crate::services::bot::{self, TelegramUpdate, TelegramUser};
use crate::services::{auth, bot_data, holdings, notify, onboarding, step_up};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

async fn app_state(pool: &PgPool, mock: &Mock) -> ApplicationState {
    let mut st = state(pool, mock, true).await;
    st.config.web_app_url = Some("https://app.example/".into());
    st
}

fn update(id: i64, body: Value) -> TelegramUpdate {
    let mut value = json!({ "update_id": id });
    value.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

fn say(id: i64, from: i64, text: &str) -> TelegramUpdate {
    update(id, json!({ "message": { "chat": { "id": from }, "from": { "id": from, "username": "ada" }, "text": text } }))
}

fn press(id: i64, from: i64, data: &str) -> TelegramUpdate {
    update(id, json!({ "callback_query": { "id": format!("cb-{id}"), "from": { "id": from, "username": "ada" }, "data": data } }))
}

fn sent(mock: &Mock) -> Vec<Value> {
    mock.telegram.lock().unwrap().clone()
}

fn texts(mock: &Mock) -> Vec<String> {
    sent(mock).iter().filter_map(|m| m["text"].as_str().map(String::from)).collect()
}

fn button_data(message: &Value) -> Vec<String> {
    message["reply_markup"]["inline_keyboard"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r[0]["callback_data"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn button_urls(message: &Value) -> Vec<String> {
    message["reply_markup"]["inline_keyboard"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r[0]["web_app"]["url"].as_str().map(String::from)).collect())
        .unwrap_or_default()
}

async fn telegram_id_of(pool: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1").bind(user).fetch_one(pool).await.unwrap()
}

async fn wait_for(mock: &Mock, count: usize) {
    for _ in 0..100 {
        if sent(mock).len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---- Bot updates ----

#[sqlx::test]
async fn updates_without_a_message_or_button_press_are_ignored(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, update(1, json!({}))).await.unwrap();
    assert!(sent(&mock).is_empty());
    let claimed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_updates").fetch_one(&pool).await.unwrap();
    assert_eq!(claimed, 0, "nothing to process means nothing is claimed");
}

#[sqlx::test]
async fn telegram_retries_are_processed_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    for _ in 0..3 {
        bot::handle_update(&st, say(77, 500, "/help")).await.unwrap();
    }
    assert_eq!(texts(&mock).len(), 1, "the same update_id answers once");
    bot::handle_update(&st, say(78, 500, "/help")).await.unwrap();
    assert_eq!(texts(&mock).len(), 2, "a new update_id is a new update");
    let claimed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_updates").fetch_one(&pool).await.unwrap();
    assert_eq!(claimed, 2);
}

#[sqlx::test]
async fn concurrent_deliveries_of_one_update_answer_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let st = st.clone();
        tasks.spawn(async move { bot::handle_update(&st, say(90, 501, "/help")).await.unwrap() });
    }
    while tasks.join_next().await.is_some() {}
    assert_eq!(texts(&mock).len(), 1);
}

#[sqlx::test]
async fn plain_chat_and_unknown_commands_get_the_right_reply(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 502, "hello there")).await.unwrap();
    assert!(sent(&mock).is_empty(), "free text is not a command");
    bot::handle_update(&st, say(2, 502, "/nonsense")).await.unwrap();
    assert!(texts(&mock)[0].contains("do not know that command"));
    bot::handle_update(&st, say(3, 502, "/help@usetenth_bot")).await.unwrap();
    assert!(texts(&mock)[1].contains("What I can do"), "commands addressed to the bot by name still work");
}

#[sqlx::test]
async fn a_message_without_a_sender_is_ignored(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, update(1, json!({ "message": { "chat": { "id": 9 }, "text": "/help" } }))).await.unwrap();
    assert!(sent(&mock).is_empty());
}

#[sqlx::test]
async fn starting_the_bot_sends_one_picture_message_that_asks_for_the_percentage(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 503, "/start")).await.unwrap();

    let messages = sent(&mock);
    assert_eq!(messages.len(), 1, "the picture already says what usetenth is, so there is no separate welcome");
    assert_eq!(messages[0]["photo"], true);
    assert!(messages[0]["text"].as_str().unwrap().contains("Set your tenth"));
    assert_eq!(button_data(&messages[0]), vec!["pct:5", "pct:10", "pct:20"]);
    assert!(button_urls(&messages[0]).is_empty(), "the app button only appears once the tenth is set");
    assert_eq!(messages[0]["parse_mode"], "HTML");
    assert_eq!(messages[0]["chat_id"], 503);

    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE telegram_id = 503").fetch_one(&pool).await.unwrap();
    assert_eq!(users, 1);
}

#[sqlx::test]
async fn a_user_who_set_their_tenth_is_not_asked_again(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 504, "/start")).await.unwrap();
    onboarding::save_rule(&st, onboarding::ensure_user(&st, 504, None).await.unwrap().user_id, 10, &InvestmentPlan::Steady).await.unwrap();
    mock.telegram.lock().unwrap().clear();

    bot::handle_update(&st, say(2, 504, "/start")).await.unwrap();
    assert!(texts(&mock).iter().all(|t| !t.contains("Set your tenth")));
}

#[sqlx::test]
async fn the_percentage_button_leads_to_the_mix_choice(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, press(1, 505, "pct:20")).await.unwrap();
    let messages = sent(&mock);
    assert_eq!(messages.len(), 1);
    assert!(messages[0]["text"].as_str().unwrap().contains("You picked 20%"));
    assert_eq!(button_data(&messages[0]), vec!["plan:20:steady", "plan:20:growth", "plan:20:tech"]);
}

#[sqlx::test]
async fn choosing_a_mix_saves_the_tenth_and_a_gateway_failure_offers_a_retry(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, press(1, 506, "plan:10:growth")).await.unwrap();

    let (percentage, plan, enabled): (i16, InvestmentPlan, bool) =
        sqlx::query_as("SELECT r.percentage, r.plan, r.enabled FROM rules r JOIN users u ON u.id = r.user_id WHERE u.telegram_id = 506")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((percentage, plan, enabled), (10, InvestmentPlan::Growth, false), "a new rule stays off until KYC passes");

    let all = texts(&mock);
    assert!(all[0].contains("Your tenth is saved"));
    assert!(all[1].contains("could not finish setting up your account"), "the test gateway has no user endpoint");
    assert_eq!(button_data(&sent(&mock)[1]), vec!["retry"]);
}

#[sqlx::test]
async fn invalid_buttons_do_nothing(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    for (i, data) in ["pct:15", "pct:", "plan:10:custom", "plan:abc:steady", "nonsense", "a:b:c:d"].iter().enumerate() {
        bot::handle_update(&st, press(i as i64 + 1, 507, data)).await.unwrap();
    }
    assert!(sent(&mock).is_empty());
    let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules").fetch_one(&pool).await.unwrap();
    assert_eq!(rules, 0);
}

#[sqlx::test]
async fn the_tenth_button_restarts_the_percentage_choice(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, press(1, 508, "tenth")).await.unwrap();
    assert_eq!(button_data(&sent(&mock)[0]), vec!["pct:5", "pct:10", "pct:20"]);
}

#[sqlx::test]
async fn a_user_waiting_on_identity_checks_is_sent_to_verify(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    sqlx::query("UPDATE users SET verified = 'unverified' WHERE id = $1").bind(user).execute(&pool).await.unwrap();
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/start")).await.unwrap();
    let first = &sent(&mock)[0];
    assert!(first["text"].as_str().unwrap().contains("Verify your identity"));
    assert_eq!(button_urls(first), vec!["https://app.example/verify"]);

    sqlx::query("INSERT INTO kyc_session_records (user_id, full_name, nin_hash, nin_masked, status) VALUES ($1, 'Ada Obi', 'h', '*******1234', 'pending')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    bot::handle_update(&st, say(2, chat, "/start")).await.unwrap();
    let second = &sent(&mock)[1];
    assert!(second["text"].as_str().unwrap().contains("checking your details"));
    assert!(second["reply_markup"].is_null(), "nothing to press while a check is running");

    sqlx::query("UPDATE kyc_session_records SET status = 'review' WHERE user_id = $1").bind(user).execute(&pool).await.unwrap();
    bot::handle_update(&st, say(3, chat, "/start")).await.unwrap();
    assert!(sent(&mock)[2]["text"].as_str().unwrap().contains("closer look"));
}

#[sqlx::test]
async fn auto_kyc_verifies_the_user_and_activates_the_tenth(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = app_state(&pool, &mock).await;
    st.config.kyc.mode = KycMode::Auto;
    let user = seed_user(&pool).await;
    sqlx::query("UPDATE users SET verified = 'unverified' WHERE id = $1").bind(user).execute(&pool).await.unwrap();
    sqlx::query("UPDATE rules SET enabled = false, enabled_at = NULL WHERE user_id = $1").bind(user).execute(&pool).await.unwrap();
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/start")).await.unwrap();
    let all = texts(&mock);
    assert!(all[0].contains("You are verified"));
    assert!(all[1].contains("Your wallet is being prepared"), "no wallet rows yet");
    assert!(all[2].contains("Before you start"), "the terms follow a finished registration");

    let (verified, enabled, enabled_at): (VerificationStatus, bool, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT u.verified, r.enabled, r.enabled_at FROM users u JOIN rules r ON r.user_id = u.id WHERE u.id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(verified, VerificationStatus::Verified);
    assert!(enabled && enabled_at.is_some());
}

#[sqlx::test]
async fn a_new_user_asking_for_the_portfolio_is_sent_through_onboarding_first(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 509, "/portfolio")).await.unwrap();
    let messages = sent(&mock);
    assert_eq!(messages.len(), 1);
    assert!(messages[0]["text"].as_str().unwrap().contains("Set your tenth"));
    for (i, command) in ["/deposit", "/address", "/buy", "/sell", "/withdraw"].iter().enumerate() {
        bot::handle_update(&st, say(10 + i as i64, 509, command)).await.unwrap();
    }
    assert!(texts(&mock).iter().all(|t| t.contains("Set your tenth")), "no money command runs before onboarding is done");
}

#[sqlx::test]
async fn an_active_user_sees_their_tenth_and_portfolio(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/tenth")).await.unwrap();
    let tenth = &sent(&mock)[0];
    assert!(tenth["text"].as_str().unwrap().contains("10% of every payment"));
    assert!(tenth["text"].as_str().unwrap().contains("Steady"));
    assert_eq!(button_data(tenth), vec!["tenth"]);
    assert_eq!(button_urls(tenth), vec!["https://app.example/tenth"]);

    bot::handle_update(&st, say(2, chat, "/portfolio")).await.unwrap();
    let portfolio = &sent(&mock)[1];
    assert!(portfolio["text"].as_str().unwrap().contains("Your investments are worth"));
    assert_eq!(button_urls(portfolio), vec!["https://app.example/portfolio"]);
}

#[sqlx::test]
async fn changing_the_tenth_in_the_bot_confirms_the_new_one(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, press(1, chat, "plan:20:tech")).await.unwrap();
    let text = texts(&mock).pop().unwrap();
    assert!(text.contains("20% of every payment") && text.contains("Tech-heavy"), "{text}");
    let (percentage, plan, enabled): (i16, InvestmentPlan, bool) =
        sqlx::query_as("SELECT percentage, plan, enabled FROM rules WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!((percentage, plan, enabled), (20, InvestmentPlan::Tech, true), "editing keeps the rule switched on");
}

#[sqlx::test]
async fn deposit_and_address_commands_show_the_wallets_with_solana_first(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/deposit")).await.unwrap();
    assert!(texts(&mock)[0].contains("Your wallet is being prepared"));

    for (network, address) in [("evm", "0xabc"), ("solana", "SoLAddr111")] {
        sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, $2, $3)").bind(user).bind(address).bind(network).execute(&pool).await.unwrap();
    }
    bot::handle_update(&st, say(2, chat, "/deposit")).await.unwrap();
    let deposit = &sent(&mock)[1];
    let text = deposit["text"].as_str().unwrap();
    assert!(text.contains("<code>SoLAddr111</code>") && !text.contains("0xabc"), "{text}");
    assert_eq!(button_urls(deposit), vec!["https://app.example/add-funds", "https://app.example/add-funds?method=bank"]);

    bot::handle_update(&st, say(3, chat, "/address")).await.unwrap();
    let addresses = texts(&mock).pop().unwrap();
    assert!(addresses.contains("SoLAddr111") && addresses.contains("0xabc"));
    assert_eq!(button_urls(&sent(&mock)[2]), vec!["https://app.example/add-funds"]);
}

#[sqlx::test]
async fn sell_lists_what_the_user_holds_with_a_button_for_each(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/sell")).await.unwrap();
    let sell = &sent(&mock)[0];
    assert!(sell["text"].as_str().unwrap().contains("SPDR S&amp;P 500, worth about $1000.00"), "names are escaped");
    assert_eq!(button_urls(sell), vec![format!("https://app.example/sell?asset={SPY}")]);

    *mock.spy_available.lock().unwrap() = "0".into();
    bot::handle_update(&st, say(2, chat, "/sell")).await.unwrap();
    let none = &sent(&mock)[1];
    assert!(none["text"].as_str().unwrap().contains("Nothing to sell yet"));
    assert_eq!(button_urls(none), vec!["https://app.example/buy"]);
}

#[sqlx::test]
async fn buy_and_withdraw_point_at_the_app(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    bot::handle_update(&st, say(1, chat, "/buy")).await.unwrap();
    bot::handle_update(&st, say(2, chat, "/withdraw")).await.unwrap();
    let messages = sent(&mock);
    assert_eq!(button_urls(&messages[0]), vec!["https://app.example/buy"]);
    assert_eq!(button_urls(&messages[1]), vec!["https://app.example/withdraw"]);
}

#[sqlx::test]
async fn without_a_web_app_url_messages_carry_no_buttons(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = app_state(&pool, &mock).await;
    st.config.web_app_url = None;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    bot::handle_update(&st, say(1, chat, "/buy")).await.unwrap();
    assert!(sent(&mock)[0]["reply_markup"].is_null());
}

#[sqlx::test]
async fn the_login_command_issues_a_code_that_signs_the_user_in_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 510, "/login")).await.unwrap();

    let text = texts(&mock).pop().unwrap();
    let start = text.find("<code>").unwrap() + "<code>".len();
    let code = &text[start..text.find("</code>").unwrap()];
    assert_eq!(code.len(), 8);

    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM telegram_login_codes WHERE code_hash = $1").bind(code).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, 0, "only the hash is stored");
    assert!(auth::telegram_login(&st, code).await.is_ok());
    assert!(auth::telegram_login(&st, code).await.is_err(), "a code works once");
}

#[sqlx::test]
async fn a_failing_command_apologises_instead_of_going_silent(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    sqlx::query("DROP TABLE rules CASCADE").execute(&pool).await.unwrap();
    bot::handle_update(&st, say(1, 511, "/start")).await.unwrap();
    assert_eq!(texts(&mock), vec!["Something went wrong. Please try again in a moment."]);
    bot::handle_update(&st, press(2, 511, "retry")).await.unwrap();
    assert_eq!(texts(&mock).len(), 2);
}

// ---- Telegram webhook ----

async fn serve_router(st: ApplicationState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::routes::app_router(st);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

#[sqlx::test]
async fn the_webhook_needs_the_secret_acknowledges_fast_and_answers_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let secret = st.config.telegram_webhook_secret.clone();
    let base = serve_router(st).await;
    let client = reqwest::Client::new();
    let body = json!({ "update_id": 1001, "message": { "chat": { "id": 600 }, "from": { "id": 600 }, "text": "/help" } });

    let none = client.post(format!("{base}/webhook")).json(&body).send().await.unwrap();
    assert_eq!(none.status(), 401, "no secret header");
    let wrong = client.post(format!("{base}/webhook")).header("x-telegram-bot-api-secret-token", "wrong").json(&body).send().await.unwrap();
    assert_eq!(wrong.status(), 401);
    assert!(sent(&mock).is_empty(), "rejected calls never reach the bot");

    for _ in 0..2 {
        let ok = client.post(format!("{base}/webhook")).header("x-telegram-bot-api-secret-token", &secret).json(&body).send().await.unwrap();
        assert_eq!(ok.status(), 200);
        let reply: Value = ok.json().await.unwrap();
        assert_eq!(reply, json!({ "success": true, "message": "ok", "data": null }));
    }
    wait_for(&mock, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(texts(&mock).len(), 1, "the retry of the same update is deduplicated");
}

#[sqlx::test]
async fn the_webhook_only_accepts_post_and_the_health_routes_stay_public(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let secret = st.config.telegram_webhook_secret.clone();
    let base = serve_router(st).await;
    let client = reqwest::Client::new();

    let get = client.get(format!("{base}/webhook")).header("x-telegram-bot-api-secret-token", &secret).send().await.unwrap();
    assert_eq!(get.status(), 405);
    let malformed = client.post(format!("{base}/webhook")).header("x-telegram-bot-api-secret-token", &secret).json(&json!({ "not": "an update" })).send().await.unwrap();
    assert_eq!(malformed.status(), 422);

    assert_eq!(client.get(format!("{base}/health")).send().await.unwrap().status(), 200);
    assert_eq!(client.get(format!("{base}/")).send().await.unwrap().text().await.unwrap(), "Welcome to usetenth rust API");
}

// ---- Bot data ----

#[sqlx::test]
async fn deposit_addresses_default_to_solana_and_are_listed_in_order(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    assert!(bot_data::deposit_for_user(&st, user).await.unwrap().is_none());

    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, '0xabc', 'evm')").bind(user).execute(&pool).await.unwrap();
    let evm_only = bot_data::deposit_for_user(&st, user).await.unwrap().unwrap();
    assert_eq!(evm_only.default_network, "evm");
    assert_eq!(evm_only.asset, "USDC");

    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, 'SoL', 'solana')").bind(user).execute(&pool).await.unwrap();
    let both = bot_data::deposit_for_user(&st, user).await.unwrap().unwrap();
    assert_eq!(both.default_network, "solana");
    assert_eq!(both.networks.iter().map(|n| n.network.as_str()).collect::<Vec<_>>(), vec!["evm", "solana"]);
}

#[sqlx::test]
async fn the_bot_rule_names_every_mix(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    let from = TelegramUser { id: chat, username: None };

    for (plan, name) in [("steady", "Steady"), ("growth", "Growth"), ("tech", "Tech-heavy"), ("custom", "Your picks")] {
        sqlx::query("UPDATE rules SET plan = $2::investment_plan, percentage = 5 WHERE user_id = $1").bind(user).bind(plan).execute(&pool).await.unwrap();
        let rule = bot_data::get_rule(&st, &from).await.unwrap().unwrap();
        assert_eq!((rule.percent, rule.mix_name.as_str()), (5, name));
    }
    let stranger = TelegramUser { id: chat + 1, username: None };
    assert!(bot_data::get_rule(&st, &stranger).await.unwrap().is_none());
}

#[sqlx::test]
async fn the_bot_portfolio_separates_cash_from_investments(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    let from = TelegramUser { id: chat, username: None };

    let portfolio = bot_data::get_portfolio(&st, &from).await.unwrap().unwrap();
    assert_eq!(portfolio.holdings.len(), 1, "the stablecoin is cash, not a holding");
    assert_eq!(portfolio.holdings[0].name, "SPDR S&P 500");
    assert_eq!(portfolio.holdings[0].amount, "2");
    assert_eq!((portfolio.cash.as_str(), portfolio.value.as_str(), portfolio.invested.as_str()), ("0.00", "0.00", "0.00"));
    assert_eq!(portfolio.gain, None);

    let no_account = TelegramUser { id: 999_999, username: None };
    assert!(bot_data::get_portfolio(&st, &no_account).await.unwrap().is_none());
    assert!(bot_data::get_sellable(&st, &no_account).await.unwrap().is_none());
}

// ---- Holdings ----

#[sqlx::test]
async fn only_tradeable_non_stable_balances_with_something_available_can_be_sold(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;

    let list = holdings::sellable(&st, "gw-any").await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].asset_key, SPY);
    assert_eq!(list[0].symbol, "SPY");
    assert_eq!(list[0].available, "2");
    assert_eq!(list[0].price.as_deref(), Some("500"));
    assert_eq!(list[0].value_usd.as_deref(), Some("1000.00"));

    *mock.spy_available.lock().unwrap() = "0.5".into();
    assert_eq!(holdings::sellable(&st, "gw-any").await.unwrap()[0].value_usd.as_deref(), Some("250.00"));
    *mock.spy_available.lock().unwrap() = "0.000".into();
    assert!(holdings::sellable(&st, "gw-any").await.unwrap().is_empty(), "a zero balance is not sellable");
}

// ---- Notices ----

#[sqlx::test]
async fn a_notice_reaches_a_telegram_user_and_skips_everyone_else(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let telegram_user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, telegram_user).await;
    let email_only: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ('mail@example.com') RETURNING id").fetch_one(&pool).await.unwrap();

    notify::tell_user(&st, email_only, "nobody home".into());
    notify::tell_user(&st, Uuid::new_v4(), "no such user".into());
    notify::tell_user(&st, telegram_user, "<b>hello</b>".into());
    wait_for(&mock, 1).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let messages = sent(&mock);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["chat_id"], chat);
    assert_eq!(messages[0]["text"], "<b>hello</b>");
}

#[sqlx::test]
async fn a_telegram_outage_never_breaks_the_caller(pool: PgPool) {
    let mock = Mock::new("complete");
    mock.telegram_down.store(true, std::sync::atomic::Ordering::SeqCst);
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    notify::tell_user(&st, user, "hello".into());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(sent(&mock).is_empty());
}

// ---- Onboarding ----

#[sqlx::test]
async fn first_contact_creates_one_user_and_later_contact_refreshes_the_username(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let first = onboarding::ensure_user(&st, 700, Some("old")).await.unwrap();
    assert_eq!(first.stage, onboarding::Stage::NeedsRule);
    let again = onboarding::ensure_user(&st, 700, Some("new")).await.unwrap();
    assert_eq!(first.user_id, again.user_id);
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE telegram_id = 700").fetch_one(&pool).await.unwrap();
    assert_eq!(username.as_deref(), Some("new"));
}

#[sqlx::test]
async fn saving_a_rule_validates_the_percentage_and_never_switches_it_on(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = onboarding::ensure_user(&st, 701, None).await.unwrap().user_id;

    assert!(onboarding::save_rule(&st, user, 15, &InvestmentPlan::Steady).await.is_err());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rules").fetch_one(&pool).await.unwrap(), 0);

    onboarding::save_rule(&st, user, 5, &InvestmentPlan::Steady).await.unwrap();
    assert_eq!(onboarding::stage_of(&st, user).await.unwrap(), onboarding::Stage::NeedsGateway);
    sqlx::query("UPDATE rules SET enabled = true WHERE user_id = $1").bind(user).execute(&pool).await.unwrap();
    onboarding::save_rule(&st, user, 20, &InvestmentPlan::Tech).await.unwrap();

    let (percentage, plan, enabled): (i16, InvestmentPlan, bool) =
        sqlx::query_as("SELECT percentage, plan, enabled FROM rules WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!((percentage, plan, enabled), (20, InvestmentPlan::Tech, true));
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rules").fetch_one(&pool).await.unwrap(), 1, "one rule per user");
}

#[sqlx::test]
async fn the_gateway_user_id_is_only_ever_set_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = onboarding::ensure_user(&st, 702, None).await.unwrap().user_id;
    onboarding::set_gateway_user_id(&st, user, "gw-first").await.unwrap();
    onboarding::set_gateway_user_id(&st, user, "gw-second").await.unwrap();
    let stored: Option<String> = sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(stored.as_deref(), Some("gw-first"));
}

#[sqlx::test]
async fn advancing_stops_at_kyc_in_vendor_mode_and_finishes_in_auto_mode(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = app_state(&pool, &mock).await;
    let user = onboarding::ensure_user(&st, 703, None).await.unwrap().user_id;
    onboarding::save_rule(&st, user, 10, &InvestmentPlan::Steady).await.unwrap();
    onboarding::set_gateway_user_id(&st, user, "gw-703").await.unwrap();

    assert_eq!(onboarding::advance(&st, user).await.unwrap(), onboarding::Stage::NeedsKyc);
    st.config.kyc.mode = KycMode::Auto;
    assert_eq!(onboarding::advance(&st, user).await.unwrap(), onboarding::Stage::Active);
    assert_eq!(onboarding::advance(&st, user).await.unwrap(), onboarding::Stage::Active, "advancing a finished user is a no-op");
}

#[sqlx::test]
async fn a_gateway_failure_leaves_the_user_where_they_were(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = onboarding::ensure_user(&st, 704, None).await.unwrap().user_id;
    onboarding::save_rule(&st, user, 10, &InvestmentPlan::Steady).await.unwrap();

    assert!(onboarding::advance(&st, user).await.is_err());
    assert_eq!(onboarding::stage_of(&st, user).await.unwrap(), onboarding::Stage::NeedsGateway);
    let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signer_keys WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(keys, 1, "the signer key survives so the retry reuses it");
    assert!(onboarding::advance(&st, user).await.is_err());
    let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signer_keys WHERE user_id = $1").bind(user).fetch_one(&pool).await.unwrap();
    assert_eq!(keys, 1);
}

// ---- Access tokens ----

#[sqlx::test]
async fn access_tokens_are_bound_to_the_secret_and_the_clock(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = app_state(&pool, &mock).await;
    let user = Uuid::new_v4();
    let token = auth::access_token_for_tests(&st, user).await;
    assert_eq!(auth::verify_access_token(&st, &token).unwrap(), user);

    assert!(auth::verify_access_token(&st, &format!("{token}x")).is_err(), "tampered");
    assert!(auth::verify_access_token(&st, "").is_err());
    assert!(auth::verify_access_token(&st, "a.b.c").is_err());

    let mut other = st.clone();
    other.config.jwt_secret = "a-completely-different-secret-of-32-bytes".into();
    assert!(auth::verify_access_token(&other, &token).is_err(), "signed with another secret");

    st.config.access_token_ttl_secs = -60;
    let expired = auth::access_token_for_tests(&st, user).await;
    assert!(auth::verify_access_token(&st, &expired).is_err());
}

#[sqlx::test]
async fn a_new_telegram_code_retires_the_previous_one_and_old_codes_expire(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let first = auth::issue_telegram_code(&st, 800, Some("ada")).await.unwrap();
    let second = auth::issue_telegram_code(&st, 800, Some("ada")).await.unwrap();
    assert!(auth::telegram_login(&st, &first).await.is_err(), "only the newest code is live");

    sqlx::query("UPDATE telegram_login_codes SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    assert!(auth::telegram_login(&st, &second).await.is_err(), "expired");
    let third = auth::issue_telegram_code(&st, 800, None).await.unwrap();
    assert!(auth::telegram_login(&st, &third).await.is_ok());
}

#[test]
fn login_codes_use_only_unambiguous_characters() {
    for _ in 0..50 {
        let code = auth::random_code(8);
        assert_eq!(code.len(), 8);
        assert!(code.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
        assert!(!code.contains(['0', 'O', '1', 'I', 'L']), "{code}");
    }
    assert_eq!(auth::sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}

// ---- Confirming irreversible actions ----

#[sqlx::test]
async fn a_user_with_nothing_to_confirm_with_cannot_withdraw(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let orphan: Uuid = sqlx::query_scalar("INSERT INTO users (email) VALUES ('x@example.com') RETURNING id").fetch_one(&pool).await.unwrap();
    assert!(step_up::method_for(&st, orphan).await.is_err());
    assert!(step_up::method_for(&st, Uuid::new_v4()).await.is_err(), "unknown users are unauthorized");

    let telegram = seed_user(&pool).await;
    assert_eq!(step_up::method_for(&st, telegram).await.unwrap(), step_up::Method::TelegramCode);
    sqlx::query("UPDATE users SET password = 'hash' WHERE id = $1").bind(telegram).execute(&pool).await.unwrap();
    assert_eq!(step_up::method_for(&st, telegram).await.unwrap(), step_up::Method::Password, "a password wins over Telegram");
}

// ---- Asset catalogue ----

#[sqlx::test]
async fn syncing_asks_for_stocks_explicitly_and_marks_what_can_be_traded(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let (synced, tradable) = crate::services::catalog::sync_assets(&st).await.unwrap();

    assert!(mock.assets_query.lock().unwrap().contains("asset_class=crypto,stock"), "the gateway lists crypto only unless told otherwise");
    assert_eq!((synced, tradable), (5 + 1, 5), "a stock that is not tradeable is stored but not offered");
    let class: String = sqlx::query_scalar("SELECT asset_class FROM assets WHERE symbol = 'MSFT'").fetch_one(&pool).await.unwrap();
    assert_eq!(class, "stock");

    crate::services::catalog::sync_assets(&st).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM assets").fetch_one(&pool).await.unwrap(), 6, "syncing again updates, never duplicates");
}

#[sqlx::test]
async fn seeding_fills_each_mix_from_the_stocks_that_exist_and_scales_to_a_whole(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    crate::services::catalog::sync_assets(&st).await.unwrap();
    let reports = crate::services::catalog::seed_plans(&st).await.unwrap();

    let steady = reports.iter().find(|r| r.plan == "steady").unwrap();
    assert_eq!(steady.used, vec!["AAPL", "MSFT"]);
    assert!(steady.missing.contains(&"KO".to_string()));
    let growth = reports.iter().find(|r| r.plan == "growth").unwrap();
    assert!(growth.used.is_empty(), "NVDA is not tradeable in the test catalogue");

    let rows: Vec<(String, i32)> = sqlx::query_as("SELECT asset_key, weight_bps FROM plan_allocations WHERE plan = 'steady' ORDER BY weight_bps DESC, asset_key").fetch_all(&pool).await.unwrap();
    assert_eq!(rows.iter().map(|(_, w)| *w).sum::<i32>(), 10_000);
    assert!(rows.iter().any(|(k, _)| k == "aaaaaaaa-0000-4000-8000-000000000002"), "the Coinbase listing on Base is preferred over Solana and Robinhood");
    assert!(rows.iter().any(|(k, _)| k == "aaaaaaaa-0000-4000-8000-000000000003"), "a company with one listing uses it");
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM plan_allocations WHERE plan = 'growth'").fetch_one(&pool).await.unwrap(), 0);

    crate::services::catalog::seed_plans(&st).await.unwrap();
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM plan_allocations WHERE plan = 'steady'").fetch_one(&pool).await.unwrap(), 2, "seeding twice replaces, never doubles");
}

#[sqlx::test]
async fn each_company_is_listed_once_with_its_best_listing(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    crate::services::catalog::sync_assets(&st).await.unwrap();
    let list = crate::services::catalog::listable(&pool).await.unwrap();

    let apples: Vec<_> = list.iter().filter(|(_, _, name, _)| name == "Apple").collect();
    assert_eq!(apples.len(), 1, "three listings of Apple show as one");
    assert_eq!(apples[0].1, "AAPLC");
    assert!(!list.iter().any(|(_, s, _, _)| s == "NVDA"), "untradeable stocks are hidden");
    let names: Vec<&str> = list.iter().map(|(_, _, n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["Apple", "Bitcoin", "Microsoft"], "sorted by name");
}

#[sqlx::test]
async fn a_quote_is_priced_in_the_stablecoin_on_the_stocks_own_network(pool: PgPool) {
    for (key, chain) in [("usdc-sol", Some("solana")), ("usdc-base", Some("base")), ("usdc-cefi", None), ("usdc-hyper", Some("hypercore-perp"))] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'USDC', 'USD Coin', $2, false, true)").bind(key).bind(chain).execute(&pool).await.unwrap();
    }
    for (key, chain) in [("stock-base", Some("base")), ("stock-sol", Some("solana")), ("stock-cefi", None), ("stock-rh", Some("robinhood"))] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable, asset_class) VALUES ($1, 'X', 'X', $2, true, false, 'stock')").bind(key).bind(chain).execute(&pool).await.unwrap();
    }
    let symbols = vec!["USDC".to_string()];
    let quote = |key: &'static str| {
        let pool = pool.clone();
        let symbols = symbols.clone();
        async move { crate::services::catalog::quote_asset_for(&pool, key, &symbols).await.unwrap() }
    };
    assert_eq!(quote("stock-base").await.as_deref(), Some("usdc-base"));
    assert_eq!(quote("stock-sol").await.as_deref(), Some("usdc-sol"));
    assert_eq!(quote("stock-cefi").await.as_deref(), Some("usdc-cefi"));
    assert_eq!(quote("stock-rh").await.as_deref(), Some("usdc-cefi"), "a network with no USDC falls back to the exchange one");
    assert_eq!(quote("missing").await, None);
}

// ---- Picture cards ----

#[sqlx::test]
async fn the_percentage_question_is_the_pictures_caption(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 520, "/start")).await.unwrap();

    let first = &sent(&mock)[0];
    assert_eq!(first["photo"], true);
    assert!(first["text"].as_str().unwrap().contains("How much of each payment should we invest for you?"));
    assert_eq!(button_data(first), vec!["pct:5", "pct:10", "pct:20"]);

    bot::handle_update(&st, press(2, 520, "tenth")).await.unwrap();
    assert!(sent(&mock)[1]["photo"].is_null(), "changing the tenth later does not repeat the welcome picture");
}

#[sqlx::test]
async fn the_tenth_card_matches_the_users_percentage_and_mix(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    bot::handle_update(&st, say(1, chat, "/tenth")).await.unwrap();
    let card = &sent(&mock)[0];
    assert_eq!(card["photo"], true);
    assert!(card["text"].as_str().unwrap().contains("10% of every payment"));
    assert_eq!(button_data(card), vec!["tenth"]);
}

#[sqlx::test]
async fn a_picture_that_cannot_be_sent_falls_back_to_the_same_words_as_text(pool: PgPool) {
    let mock = Mock::new("complete");
    mock.photo_down.store(true, std::sync::atomic::Ordering::SeqCst);
    let st = app_state(&pool, &mock).await;
    bot::handle_update(&st, say(1, 521, "/start")).await.unwrap();

    let first = &sent(&mock)[0];
    assert!(first["photo"].is_null());
    assert!(first["text"].as_str().unwrap().contains("Set your tenth"));
    assert_eq!(button_data(first), vec!["pct:5", "pct:10", "pct:20"]);
}

#[sqlx::test]
async fn once_verified_the_user_is_sent_to_the_app_for_their_wallet_not_shown_the_address(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = app_state(&pool, &mock).await;
    st.config.kyc.mode = KycMode::Auto;
    let user = seed_user(&pool).await;
    sqlx::query("UPDATE users SET verified = 'unverified' WHERE id = $1").bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, 'SoLAddr111', 'solana')").bind(user).execute(&pool).await.unwrap();
    let chat = telegram_id_of(&pool, user).await;

    bot::handle_update(&st, say(1, chat, "/start")).await.unwrap();
    let all = sent(&mock);
    assert!(all[0]["text"].as_str().unwrap().contains("You are verified"));
    assert!(all[1]["text"].as_str().unwrap().contains("Your wallet is ready"));
    assert!(!all[1]["text"].as_str().unwrap().contains("SoLAddr111"), "the address is shown in the app, where it can be copied and shared");
    assert_eq!(button_urls(&all[1]), vec!["https://app.example/add-funds"]);
    assert_eq!(all.len(), 3);
    assert!(all[2]["text"].as_str().unwrap().contains("Before you start"));
    assert_eq!(button_urls(&all[2]), vec!["https://app.example/terms"]);

    mock.telegram.lock().unwrap().clear();
    bot::handle_update(&st, say(2, chat, "/start")).await.unwrap();
    assert!(texts(&mock).iter().all(|t| !t.contains("Before you start")), "the terms are sent once, when registration finishes");
}

// ---- In-app notifications ----

#[sqlx::test]
async fn the_app_lists_notifications_and_marks_them_read(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let other = seed_user(&pool).await;
    for (who, title) in [(user, "Payment received"), (user, "Payment received"), (other, "Not yours")] {
        sqlx::query("INSERT INTO notifications (user_id, kind, title, body) VALUES ($1, 'deposit', $2, '$5.00 USDC just arrived in your wallet.')").bind(who).bind(title).execute(&pool).await.unwrap();
    }
    let token = auth::access_token_for_tests(&st, user).await;
    let base = serve_router(st).await;
    let client = reqwest::Client::new();

    let body: Value = client.get(format!("{base}/v1/notifications")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["data"]["unread"], 2);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 2, "only the user's own");
    assert_eq!(body["data"]["items"][0]["read"], false);

    let read = client.post(format!("{base}/v1/notifications/read")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(read.status(), 200);
    let after: Value = client.get(format!("{base}/v1/notifications")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(after["data"]["unread"], 0);
    assert_eq!(after["data"]["items"][0]["read"], true);

    assert_eq!(client.get(format!("{base}/v1/notifications")).send().await.unwrap().status(), 401);
}

// ---- Withdrawable assets ----

#[sqlx::test]
async fn stablecoins_can_be_withdrawn_even_though_the_gateway_marks_them_not_tradable(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    for (key, chain) in [("usdc-sol", Some("solana")), ("usdc-base", Some("base")), ("usdc-arb", Some("arbitrum")), ("usdc-hyper", Some("hypercore-spot")), ("usdc-cefi", None)] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'USDC', 'USD Coin', $2, false, true)").bind(key).bind(chain).execute(&pool).await.unwrap();
    }
    let user = seed_user(&pool).await;
    let token = auth::access_token_for_tests(&st, user).await;
    let base = serve_router(st).await;

    let body: Value = reqwest::Client::new().get(format!("{base}/v1/withdraw/assets")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    let mut chains: Vec<(String, String)> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["chain"].as_str().unwrap().to_string(), a["chain_family"].as_str().unwrap().to_string()))
        .collect();
    chains.sort();
    assert_eq!(chains, vec![("base".to_string(), "evm".to_string()), ("solana".to_string(), "solana".to_string())], "only the networks the wallets use");
}

// ---- Approving in Telegram ----

#[sqlx::test]
async fn a_telegram_button_approves_exactly_one_action_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let chat = telegram_id_of(&pool, user).await;
    let purpose = step_up::add_address_purpose("SoLAddr111");

    let (method, id) = step_up::challenge(&st, user, &purpose, "<b>Confirm</b>").await.unwrap();
    assert_eq!(method, step_up::Method::TelegramCode);
    let id = id.unwrap();
    let request = sent(&mock).pop().unwrap();
    assert_eq!(button_data(&request), vec![format!("ok:{id}")], "the message carries an Approve button");
    assert!(request["text"].as_str().unwrap().contains("confirmation code"), "and the code stays as a fallback");

    assert_eq!(step_up::status(&st, user, id).await.unwrap(), (false, true));
    assert!(step_up::confirm(&st, user, &purpose, None, None, Some(id)).await.is_err(), "not approved yet");

    bot::handle_update(&st, press(1, chat, &format!("ok:{id}"))).await.unwrap();
    assert!(texts(&mock).last().unwrap().contains("Approved"));
    assert_eq!(step_up::status(&st, user, id).await.unwrap(), (true, true));

    assert!(step_up::confirm(&st, user, &step_up::add_address_purpose("Other"), None, None, Some(id)).await.is_err(), "bound to its own action");
    assert!(step_up::confirm(&st, user, &purpose, None, None, Some(id)).await.is_ok());
    assert!(step_up::confirm(&st, user, &purpose, None, None, Some(id)).await.is_err(), "an approval works once");
}

#[sqlx::test]
async fn only_the_owner_can_approve_and_expired_requests_cannot_be_approved(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    let stranger = seed_user(&pool).await;
    let (stranger_chat, chat) = (telegram_id_of(&pool, stranger).await, telegram_id_of(&pool, user).await);
    let purpose = step_up::add_address_purpose("SoLAddr111");
    let id = step_up::challenge(&st, user, &purpose, "x").await.unwrap().1.unwrap();

    bot::handle_update(&st, press(1, stranger_chat, &format!("ok:{id}"))).await.unwrap();
    assert!(texts(&mock).last().unwrap().contains("expired"), "someone else's tap does nothing");
    assert_eq!(step_up::status(&st, user, id).await.unwrap().0, false);
    assert!(step_up::status(&st, stranger, id).await.is_err(), "and they cannot even see it");

    sqlx::query("UPDATE action_confirmations SET expires_at = now() - interval '1 second'").execute(&pool).await.unwrap();
    bot::handle_update(&st, press(2, chat, &format!("ok:{id}"))).await.unwrap();
    assert!(texts(&mock).last().unwrap().contains("expired"));
    assert!(step_up::confirm(&st, user, &purpose, None, None, Some(id)).await.is_err());
}

#[sqlx::test]
async fn a_password_user_is_never_asked_to_approve_in_telegram(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = app_state(&pool, &mock).await;
    let user = seed_user(&pool).await;
    sqlx::query("UPDATE users SET password = 'hash' WHERE id = $1").bind(user).execute(&pool).await.unwrap();
    let (method, id) = step_up::challenge(&st, user, "p", "x").await.unwrap();
    assert_eq!((method, id), (step_up::Method::Password, None));
    assert!(sent(&mock).is_empty());
}
