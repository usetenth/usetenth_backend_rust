use crate::app_state::ApplicationState;
use crate::models::schemas::InvestmentPlan;
use crate::services::bot_data::{self, BotDeposit, BotPortfolio, BotRule};
use crate::services::onboarding::{self, Stage};
use crate::services::telegram_service::{Button, TelegramService};
use crate::services::auth;
use crate::config::KycMode;
use crate::services::{cards, kyc, step_up};
use crate::utils::{escape_html as h, usd};
use serde::Deserialize;


#[derive(Deserialize, Clone, Debug)]
pub struct TelegramUser {
    pub id: i64,
    pub username: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Chat {
    pub id: i64,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Message {
    pub chat: Chat,
    pub from: Option<TelegramUser>,
    pub text: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct CallbackQuery {
    pub id: String,
    pub from: TelegramUser,
    // The message the pressed button was attached to.
    pub message: Option<Message>,
    pub data: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

// `/portfolio@my_bot arg` -> `portfolio`
pub fn parse_command(text: Option<&str>) -> Option<String> {
    let first = text?.split_whitespace().next()?;
    let rest = first.strip_prefix('/')?;
    let name = rest.split('@').next()?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    Some(name.to_lowercase())
}

// Copy. Plain language, no emoji; HTML for structure.
pub mod copy {
    use super::*;

    pub fn help() -> String {
        [
            "<b>What I can do</b>",
            "",
            "/portfolio  See what you own and what is ready to withdraw",
            "/tenth  Choose how much of each payment to invest, and in what",
            "/buy  Buy a small amount of a real stock",
            "/sell  Sell what you bought",
            "/deposit  How to add funds, with your deposit address",
            "/address  Just your deposit addresses",
            "/login  Get a one-time code to sign in to the mobile app",
            "/withdraw  Take money out",
            "",
            "<blockquote>Open the app from any message to do more without leaving Telegram.</blockquote>",
        ]
        .join("\n")
    }

    pub fn tenth(rule: Option<&BotRule>) -> String {
        match rule {
            Some(r) => format!(
                "<b>Your tenth</b>\n\n{}% of every payment you receive goes into your <b>{}</b> mix. The other {}% stays yours.\n\nOpen the app to change the percentage, switch mix or pick your own stocks.",
                r.percent,
                h(&r.mix_name),
                100 - r.percent.clamp(0, 100)
            ),
            None => "<b>Your tenth</b>\n\nYou have not set your tenth yet.".into(),
        }
    }

    pub fn portfolio(p: &BotPortfolio) -> String {
        if p.holdings.is_empty() {
            return format!(
                "<b>Your portfolio</b>\n\nYou have {} ready to withdraw and nothing invested yet.\n\nYour first payment will be split across your mix automatically. Open the app to check your mix or add funds.",
                usd(&p.cash)
            );
        }
        let gain = match &p.gain {
            Some(g) => {
                let dir = if g.starts_with('-') { "down" } else { "up" };
                format!(", {dir} {} since you started", usd(g.trim_start_matches('-')))
            }
            None => String::new(),
        };
        format!(
            "<b>Your portfolio</b>\n\nYour investments are worth {} today{gain}. {} is ready to withdraw.\n\n<blockquote>Open the app to see every holding and what you paid for it.</blockquote>",
            usd(&p.value),
            usd(&p.cash)
        )
    }

    pub fn withdraw() -> String {
        "<b>Withdraw</b>\n\nChoose an amount and where to send it. We withdraw money that was never invested, so nothing has to be sold.".into()
    }

    pub fn buy() -> String {
        "<b>Buy a stock</b>\n\nPick a stock and an amount, see a live quote, then confirm.".into()
    }

    pub fn sell_none() -> String {
        "<b>Nothing to sell yet</b>\n\nYou do not hold any stocks yet. Once you do, they will show up here.".into()
    }

    pub fn sell(items: &[bot_data::BotSellable]) -> String {
        let mut lines = vec![
            "<b>Sell a holding</b>".to_string(),
            String::new(),
            "Choose what to sell. You will see a live quote and confirm before anything happens.".into(),
            String::new(),
        ];
        for i in items {
            match &i.value {
                Some(v) => lines.push(format!("{}, worth about {}", h(&i.name), usd(v))),
                None => lines.push(h(&i.name)),
            }
        }
        lines.join("\n")
    }

    pub fn choose_percentage() -> String {
        "<b>Set your tenth</b>\n\nHow much of each payment should we invest for you? The rest always stays yours.".into()
    }

    pub fn choose_plan(percent: i16) -> String {
        format!("<b>Choose your mix</b>\n\nYou picked {percent}%. What should we invest it in?")
    }

    pub fn rule_saved() -> String {
        "<b>Your tenth is saved</b>\n\nNext we set up your account.".into()
    }

    pub fn gateway_pending() -> String {
        "<b>We could not finish setting up your account</b>\n\nYour tenth is saved. Tap below to try again.".into()
    }

    pub fn kyc_needed() -> String {
        "<b>Verify your identity</b>\n\nWe need your name and NIN to switch your tenth on. It takes about a minute, and your NIN is never stored in the clear.".into()
    }

    pub fn kyc_pending() -> String {
        "<b>We are checking your details</b>\n\nThis usually takes a few seconds. We will message you here as soon as it is done.".into()
    }

    pub fn kyc_review() -> String {
        "<b>We are taking a closer look</b>\n\nYour details need a quick manual check. We will message you here as soon as it is done.".into()
    }

    pub fn kyc_done() -> String {
        "<b>You are verified</b>\n\nYour tenth is now active.".into()
    }

    pub fn wallet_pending() -> String {
        "<b>Your wallet is being prepared</b>\n\nYour deposit address will show here as soon as it is ready. Send /address to check.".into()
    }

    pub fn login_code(code: &str) -> String {
        format!(
            "<b>Your app code</b>\n\n<code>{code}</code>\n\nEnter it in the app within 5 minutes. It works once.\n\n<blockquote>Never share this code. We will never ask for it in a chat.</blockquote>"
        )
    }

    pub fn wallet_ready() -> String {
        "<b>Your wallet is ready</b>\n\nOpen usetenth to see your wallet address, copy it or share it, and start receiving payments. We invest your tenth as each payment lands.".into()
    }

    pub fn terms() -> String {
        "<b>Before you start</b>\n\nSee how usetenth works, what we do with your money and your details, and the terms we operate under. It takes two minutes.".into()
    }

    pub fn approved() -> String {
        "<b>Approved</b>\n\nGo back to usetenth to finish.".into()
    }

    pub fn approval_expired() -> String {
        "<b>That request has expired</b>\n\nStart it again in usetenth.".into()
    }

    pub fn unavailable() -> String {
        "<b>Not available yet</b>\n\nThat is not switched on for your account yet. Please try again later.".into()
    }

    pub fn deposit(d: &BotDeposit) -> String {
        let Some(net) = d.networks.iter().find(|n| n.network == d.default_network) else {
            return unavailable();
        };
        format!(
            "<b>Add funds</b>\n\nSend <b>{asset}</b> to this address on the <b>{net_name}</b> network:\n\n<code>{addr}</code>\n\n<blockquote>Only send {asset} on {net_name}. Any other asset or network can be lost for good.</blockquote>\n\nWhen your payment lands we keep your tenth and invest it, and you will get a message here.",
            asset = h(&d.asset),
            net_name = h(&net.network),
            addr = h(&net.address),
        )
    }

    pub fn addresses(d: &BotDeposit) -> String {
        if d.networks.is_empty() {
            return unavailable();
        }
        let mut lines = vec!["<b>Your deposit addresses</b>".to_string(), String::new()];
        for n in &d.networks {
            lines.push(format!("<b>{}</b>, {}", h(&n.network), h(&d.asset)));
            lines.push(format!("<code>{}</code>", h(&n.address)));
            lines.push(String::new());
        }
        lines.push("Always check the network before you send.".into());
        lines.join("\n")
    }
}


// Telegram retries webhooks, so an update is processed once; returns false if already claimed.
async fn claim_update(state: &ApplicationState, update_id: i64) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO telegram_updates (update_id) VALUES ($1) ON CONFLICT (update_id) DO NOTHING",
    )
    .bind(update_id)
    .execute(&state.pool)
    .await?;
    Ok(result.rows_affected() == 1)
}


pub(crate) fn web_button(state: &ApplicationState, path: &str, text: &str) -> Vec<Button> {
    match &state.config.web_app_url {
        Some(base) => vec![Button::WebApp {
            text: text.into(),
            url: format!("{}{}", base.trim_end_matches('/'), path),
        }],
        None => vec![],
    }
}

async fn send(state: &ApplicationState, chat_id: i64, text: &str, buttons: &[Button]) -> anyhow::Result<()> {
    TelegramService::send_message(state, chat_id, text, buttons).await
}

// A picture with the text as its caption; the same words go out as plain text if the picture cannot be sent.
async fn send_card(state: &ApplicationState, chat_id: i64, card: Option<&'static [u8]>, text: &str, buttons: &[Button]) -> anyhow::Result<()> {
    if let Some(png) = card {
        match TelegramService::send_photo(state, chat_id, png, text, buttons).await {
            Ok(()) => return Ok(()),
            Err(error) => eprintln!("Card failed, sending text instead: {error:#}"),
        }
    }
    send(state, chat_id, text, buttons).await
}

fn percentage_buttons() -> Vec<Button> {
    onboarding::PERCENTAGES
        .iter()
        .map(|p| Button::callback(&format!("{p}%"), &format!("pct:{p}")))
        .collect()
}

fn plan_buttons(percent: i16) -> Vec<Button> {
    [("Steady", "steady"), ("Growth", "growth"), ("Tech-heavy", "tech")]
        .iter()
        .map(|(label, key)| Button::callback(label, &format!("plan:{percent}:{key}")))
        .collect()
}

pub async fn handle_update(state: &ApplicationState, update: TelegramUpdate) -> anyhow::Result<()> {
    if update.message.is_none() && update.callback_query.is_none() {
        return Ok(());
    }
    if !claim_update(state, update.update_id).await? {
        return Ok(());
    }

    if let Some(callback) = update.callback_query {
        let chat_id = callback.message.as_ref().map(|m| m.chat.id).unwrap_or(callback.from.id);
        let _ = TelegramService::answer_callback_query(state, &callback.id).await;
        let data = callback.data.clone().unwrap_or_default();
        if let Err(error) = run_callback(state, &data, chat_id, &callback.from).await {
            eprintln!("Telegram callback failed: {data}: {error:#}");
            let _ = send(state, chat_id, "Something went wrong. Please try again in a moment.", &[]).await;
        }
        return Ok(());
    }

    let Some(message) = update.message else { return Ok(()) };
    let Some(command) = parse_command(message.text.as_deref()) else { return Ok(()) };
    let Some(from) = message.from else { return Ok(()) };
    let chat_id = message.chat.id;

    if let Err(error) = run_command(state, &command, chat_id, &from).await {
        eprintln!("Telegram command failed: /{command}: {error:#}");
        let _ = send(state, chat_id, "Something went wrong. Please try again in a moment.", &[]).await;
    }
    Ok(())
}

// Moves the user to their next onboarding step, running automatic steps on the way.
async fn continue_onboarding(state: &ApplicationState, chat_id: i64, from: &TelegramUser) -> anyhow::Result<()> {
    let mut verified_now = false;
    // Each pass advances one stage; the cap guards against a stage that never moves.
    for _ in 0..4 {
        let ob = onboarding::ensure_user(state, from.id, from.username.as_deref()).await?;
        match ob.stage {
            Stage::NeedsRule => {
                return send_card(state, chat_id, Some(cards::welcome()), &copy::choose_percentage(), &percentage_buttons()).await;
            }
            Stage::NeedsGateway => match onboarding::provision_gateway_user(state, ob.user_id).await {
                Ok(gateway_user_id) => onboarding::set_gateway_user_id(state, ob.user_id, &gateway_user_id).await?,
                Err(error) => {
                    eprintln!("Gateway provisioning failed for user {}: {error:#}", ob.user_id);
                    return send(state, chat_id, &copy::gateway_pending(), &[Button::callback("Try again", "retry")]).await;
                }
            },
            Stage::NeedsKyc => {
                if state.config.kyc.mode == KycMode::Auto {
                    kyc::auto_approve(state, ob.user_id).await?;
                    verified_now = true;
                } else {
                    let status = kyc::current(state, ob.user_id).await.map_err(|e| anyhow::anyhow!("{e}"))?.status;
                    let text = match status.as_str() {
                        "pending" => copy::kyc_pending(),
                        "review" => copy::kyc_review(),
                        _ => copy::kyc_needed(),
                    };
                    let buttons = if status == "pending" || status == "review" { vec![] } else { web_button(state, "/verify", "Verify my identity") };
                    return send(state, chat_id, &text, &buttons).await;
                }
            }
            Stage::Active => {
                if verified_now {
                    send(state, chat_id, &copy::kyc_done(), &[]).await?;
                }
                // The address itself lives in the app, where it can be copied and shared.
                match bot_data::get_deposit(state, from).await? {
                    Some(_) => send(state, chat_id, &copy::wallet_ready(), &web_button(state, "/add-funds", "Open my wallet")).await?,
                    None => send(state, chat_id, &copy::wallet_pending(), &[]).await?,
                }
                if verified_now {
                    send(state, chat_id, &copy::terms(), &web_button(state, "/terms", "Read terms and policies")).await?;
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

// True when the user has finished onboarding, otherwise sends them to their next step.
async fn require_active(state: &ApplicationState, chat_id: i64, from: &TelegramUser) -> anyhow::Result<bool> {
    let ob = onboarding::ensure_user(state, from.id, from.username.as_deref()).await?;
    if ob.stage == Stage::Active {
        return Ok(true);
    }
    continue_onboarding(state, chat_id, from).await?;
    Ok(false)
}

async fn run_callback(state: &ApplicationState, data: &str, chat_id: i64, from: &TelegramUser) -> anyhow::Result<()> {
    let parts: Vec<&str> = data.split(':').collect();
    match parts.as_slice() {
        ["pct", p] => match onboarding::parse_percentage(p) {
            Some(percent) => send(state, chat_id, &copy::choose_plan(percent), &plan_buttons(percent)).await,
            None => Ok(()),
        },
        ["plan", p, plan] => {
            let (Some(percent), Some(plan)) = (onboarding::parse_percentage(p), onboarding::parse_plan(plan)) else {
                return Ok(());
            };
            save_and_continue(state, chat_id, from, percent, &plan).await
        }
        ["retry"] => continue_onboarding(state, chat_id, from).await,
        ["ok", id] => {
            let Ok(id) = id.parse::<uuid::Uuid>() else { return Ok(()) };
            let text = if step_up::approve(state, from.id, id).await.map_err(|e| anyhow::anyhow!("{e}"))? {
                copy::approved()
            } else {
                copy::approval_expired()
            };
            send(state, chat_id, &text, &[]).await
        }
        ["tenth"] => send(state, chat_id, &copy::choose_percentage(), &percentage_buttons()).await,
        _ => Ok(()),
    }
}

async fn save_and_continue(
    state: &ApplicationState,
    chat_id: i64,
    from: &TelegramUser,
    percent: i16,
    plan: &InvestmentPlan,
) -> anyhow::Result<()> {
    let ob = onboarding::ensure_user(state, from.id, from.username.as_deref()).await?;
    onboarding::save_rule(state, ob.user_id, percent, plan).await?;
    if ob.stage == Stage::Active {
        // An existing, active user just changed their tenth.
        let rule = bot_data::get_rule(state, from).await?;
        return send(state, chat_id, &copy::tenth(rule.as_ref()), &[]).await;
    }
    send(state, chat_id, &copy::rule_saved(), &[]).await?;
    continue_onboarding(state, chat_id, from).await
}

async fn run_command(state: &ApplicationState, command: &str, chat_id: i64, from: &TelegramUser) -> anyhow::Result<()> {
    match command {
        "start" => {
            continue_onboarding(state, chat_id, from).await
        }
        "help" => send(state, chat_id, &copy::help(), &[]).await,
        "login" => {
            let code = auth::issue_telegram_code(state, from.id, from.username.as_deref()).await?;
            send(state, chat_id, &copy::login_code(&code), &[]).await
        }
        "tenth" => {
            let ob = onboarding::ensure_user(state, from.id, from.username.as_deref()).await?;
            if ob.stage == Stage::NeedsRule {
                return continue_onboarding(state, chat_id, from).await;
            }
            let rule = bot_data::get_rule(state, from).await?;
            let mut buttons = vec![Button::callback("Change my tenth", "tenth")];
            buttons.extend(web_button(state, "/tenth", "Open my tenth"));
            let card = rule.as_ref().and_then(|r| cards::tenth(r.percent, &r.mix_name));
            send_card(state, chat_id, card, &copy::tenth(rule.as_ref()), &buttons).await
        }
        "portfolio" | "deposit" | "address" | "buy" | "sell" | "withdraw" => {
            if !require_active(state, chat_id, from).await? {
                return Ok(());
            }
            run_active_command(state, command, chat_id, from).await
        }
        _ => send(state, chat_id, "I do not know that command. Send /help to see what I can do.", &[]).await,
    }
}

// Commands that need a finished onboarding (verified user, active rule).
async fn run_active_command(state: &ApplicationState, command: &str, chat_id: i64, from: &TelegramUser) -> anyhow::Result<()> {
    match command {
        "portfolio" => {
            let buttons = web_button(state, "/portfolio", "Open my portfolio");
            match bot_data::get_portfolio(state, from).await? {
                Some(p) => send(state, chat_id, &copy::portfolio(&p), &buttons).await,
                None => send(state, chat_id, &copy::unavailable(), &buttons).await,
            }
        }
        "deposit" | "address" => {
            let Some(deposit) = bot_data::get_deposit(state, from).await? else {
                return send(state, chat_id, &copy::wallet_pending(), &[]).await;
            };
            let text = if command == "deposit" { copy::deposit(&deposit) } else { copy::addresses(&deposit) };
            let mut buttons = web_button(state, "/add-funds", "Use a different network");
            if command == "deposit" {
                buttons.extend(web_button(state, "/add-funds?method=bank", "Add funds from a bank"));
            }
            send(state, chat_id, &text, &buttons).await
        }
        "buy" => send(state, chat_id, &copy::buy(), &web_button(state, "/buy", "Buy a stock")).await,
        "sell" => match bot_data::get_sellable(state, from).await? {
            None => send(state, chat_id, &copy::unavailable(), &[]).await,
            Some(items) if items.is_empty() => {
                send(state, chat_id, &copy::sell_none(), &web_button(state, "/buy", "Buy a stock")).await
            }
            Some(items) => {
                let buttons: Vec<Button> = items
                    .iter()
                    .flat_map(|i| {
                        web_button(
                            state,
                            &format!("/sell?asset={}", url::form_urlencoded::byte_serialize(i.asset_key.as_bytes()).collect::<String>()),
                            &format!("Sell {}", i.name),
                        )
                    })
                    .collect();
                send(state, chat_id, &copy::sell(&items), &buttons).await
            }
        },
        "withdraw" => send(state, chat_id, &copy::withdraw(), &web_button(state, "/withdraw", "Open withdraw")).await,
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command(Some("/start abc")).as_deref(), Some("start"));
        assert_eq!(parse_command(Some("/Portfolio@my_bot")).as_deref(), Some("portfolio"));
        assert_eq!(parse_command(Some("hello")), None);
        assert_eq!(parse_command(Some("/")), None);
        assert_eq!(parse_command(None), None);
    }

    #[test]
    fn commands_reject_anything_that_is_not_a_plain_word() {
        assert_eq!(parse_command(Some("/with-dash")), None);
        assert_eq!(parse_command(Some("/start123")), None);
        assert_eq!(parse_command(Some("  /Help  ")).as_deref(), Some("help"));
        assert_eq!(parse_command(Some("/@bot")), None);
        assert_eq!(parse_command(Some("/sell_all now")).as_deref(), Some("sell_all"));
        assert_eq!(parse_command(Some("")), None);
    }

    fn portfolio(holdings: usize, gain: Option<&str>) -> BotPortfolio {
        BotPortfolio {
            cash: "12.50".into(),
            value: "300.00".into(),
            invested: "250.00".into(),
            gain: gain.map(String::from),
            holdings: (0..holdings).map(|i| bot_data::BotHolding { name: format!("Stock {i}"), amount: "1".into() }).collect(),
        }
    }

    #[test]
    fn the_help_lists_every_command() {
        let help = copy::help();
        for command in ["/portfolio", "/tenth", "/buy", "/sell", "/deposit", "/address", "/login", "/withdraw"] {
            assert!(help.contains(command), "{command}");
        }
    }

    #[test]
    fn the_wallet_prompt_sends_people_to_the_app_instead_of_showing_the_address() {
        let text = copy::wallet_ready();
        assert!(text.contains("copy it or share it"));
        assert!(!text.contains("<code>"));
    }

    #[test]
    fn the_tenth_reads_back_the_split_and_escapes_the_mix_name() {
        let rule = BotRule { percent: 10, mix_name: "Fun <&> picks".into() };
        let text = copy::tenth(Some(&rule));
        assert!(text.contains("10% of every payment") && text.contains("The other 90% stays yours"));
        assert!(text.contains("Fun &lt;&amp;&gt; picks"));
        assert!(copy::tenth(None).contains("not set your tenth yet"));
        let odd = BotRule { percent: 150, mix_name: "x".into() };
        assert!(copy::tenth(Some(&odd)).contains("The other 0% stays yours"), "never a negative share");
    }

    #[test]
    fn an_empty_portfolio_talks_about_cash_only() {
        let text = copy::portfolio(&portfolio(0, None));
        assert!(text.contains("$12.50 ready to withdraw and nothing invested yet"));
    }

    #[test]
    fn a_portfolio_with_holdings_says_up_down_or_nothing_about_the_gain() {
        assert!(copy::portfolio(&portfolio(2, Some("20.00"))).contains("worth $300.00 today, up $20.00 since you started"));
        assert!(copy::portfolio(&portfolio(2, Some("-5.25"))).contains("down $5.25 since you started"));
        let flat = copy::portfolio(&portfolio(2, None));
        assert!(!flat.contains("since you started") && flat.contains("$12.50 is ready to withdraw"));
    }

    fn deposit(default: &str, networks: &[(&str, &str)]) -> BotDeposit {
        BotDeposit {
            asset: "USDC".into(),
            default_network: default.into(),
            networks: networks
                .iter()
                .map(|(network, address)| bot_data::BotNetworkAddress { network: network.to_string(), address: address.to_string() })
                .collect(),
        }
    }

    #[test]
    fn deposit_copy_names_the_asset_network_and_address_and_warns_about_mistakes() {
        let text = copy::deposit(&deposit("solana", &[("evm", "0xabc"), ("solana", "SoL1")]));
        assert!(text.contains("Send <b>USDC</b>") && text.contains("<b>solana</b>"));
        assert!(text.contains("<code>SoL1</code>") && !text.contains("0xabc"));
        assert!(text.contains("Any other asset or network can be lost for good"));
        assert!(copy::deposit(&deposit("base", &[("solana", "SoL1")])).contains("Not available yet"), "no address for the default network");
    }

    #[test]
    fn the_address_list_shows_every_network_or_says_it_is_unavailable() {
        let text = copy::addresses(&deposit("solana", &[("evm", "0xabc"), ("solana", "SoL1")]));
        assert!(text.contains("<code>0xabc</code>") && text.contains("<code>SoL1</code>"));
        assert!(text.ends_with("Always check the network before you send."));
        assert!(copy::addresses(&deposit("solana", &[])).contains("Not available yet"));
    }

    #[test]
    fn the_sell_list_prices_what_it_can_and_escapes_names() {
        let items = vec![
            bot_data::BotSellable { asset_key: "a".into(), name: "A & B".into(), value: Some("10.00".into()) },
            bot_data::BotSellable { asset_key: "b".into(), name: "Plain".into(), value: None },
        ];
        let text = copy::sell(&items);
        assert!(text.contains("A &amp; B, worth about $10.00"));
        assert!(text.lines().any(|l| l == "Plain"));
        assert!(copy::sell_none().contains("Nothing to sell yet"));
    }

    #[test]
    fn the_login_code_is_shown_in_a_copyable_block_with_a_warning() {
        let text = copy::login_code("ABCD2345");
        assert!(text.contains("<code>ABCD2345</code>") && text.contains("Never share this code"));
    }

    #[test]
    fn every_onboarding_message_has_a_heading_and_no_stray_html() {
        let all = [
            copy::choose_percentage(),
            copy::choose_plan(10),
            copy::rule_saved(),
            copy::gateway_pending(),
            copy::kyc_needed(),
            copy::kyc_pending(),
            copy::kyc_review(),
            copy::kyc_done(),
            copy::wallet_pending(),
            copy::wallet_ready(),
            copy::unavailable(),
            copy::buy(),
            copy::withdraw(),
        ];
        for text in all {
            assert!(text.starts_with("<b>") && text.contains("</b>"), "{text}");
            assert_eq!(text.matches("<b>").count(), text.matches("</b>").count(), "{text}");
            assert_eq!(text.matches("<blockquote>").count(), text.matches("</blockquote>").count(), "{text}");
        }
        assert!(copy::choose_plan(20).contains("You picked 20%"));
    }

    #[test]
    fn updates_parse_from_telegrams_json_and_ignore_unknown_fields() {
        let update: TelegramUpdate = serde_json::from_value(serde_json::json!({
            "update_id": 5,
            "message": { "message_id": 1, "date": 0, "chat": { "id": 9, "type": "private" },
                         "from": { "id": 9, "is_bot": false, "first_name": "Ada", "username": "ada" }, "text": "/start" }
        }))
        .unwrap();
        assert_eq!(update.update_id, 5);
        let message = update.message.unwrap();
        assert_eq!((message.chat.id, message.from.unwrap().username.as_deref(), message.text.as_deref()), (9, Some("ada"), Some("/start")));
        assert!(update.callback_query.is_none());
    }
}
