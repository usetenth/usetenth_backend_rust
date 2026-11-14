# usetenth

Keep a tenth of every payment you receive, and have it invested for you in stocks.

usetenth is the server behind a Telegram bot and a mobile app. People who get paid in stablecoins (digital dollars) choose how much of each payment to set aside: 5%, 10% or 20%. They also choose what to invest it in. When money arrives, the server buys the stocks for them through [True Markets](https://truemarkets.co). The rest of the payment is never touched.

## The idea in five steps

1. **Sign up.** The user starts the Telegram bot or creates an account in the mobile app, picks their tenth, and proves who they are with their national ID number (NIN).
2. **Get a wallet.** The server opens a wallet for the user at True Markets. The user receives a deposit address.
3. **Get paid.** The user's clients send stablecoins to that address.
4. **Invest the tenth.** The server notices the payment, works out the user's share, and buys the chosen stocks.
5. **Stay informed.** The user gets a Telegram message, and can see everything in the app. They can also buy, sell and withdraw by hand.

## What users can do

| Feature | What it means |
| --- | --- |
| Choose a tenth | 5%, 10% or 20% of each payment, split across a mix: Steady, Growth, Tech-heavy, or their own picks. |
| Automatic investing | Every new payment is invested by the user's rule. Money that arrived before the rule was switched on is never invested later. |
| Buy and sell by hand | Get a live price first, then confirm. Pressing the button twice never places two orders. |
| Withdraw | Send money to a saved address. A new address waits 24 hours before it can be used, and each withdrawal needs the user's password or a code sent on Telegram. |
| Telegram bot | `/start` `/tenth` `/portfolio` `/deposit` `/address` `/buy` `/sell` `/withdraw` `/login` `/help` |
| Mobile app | Sign in with email and password, with a one-time code from the bot, or by opening the app inside Telegram. |

## How the server is built

The server is one program with four parts that work together.

```text
   Telegram                Mobile app / Web app
      |                            |
      v                            v
+----------------------------------------------------------+
|  1. FRONT DOOR  (answers requests)                       |
|     /webhook for Telegram, /auth and /v1 for the app,    |
|     /admin for the operator                              |
+----------------------------------------------------------+
|  2. SERVICES  (the rules of the product)                 |
|     sign-in, sign-up steps, identity check, bot replies, |
|     confirmation codes, rate limits                      |
+----------------------------------------------------------+
|  3. ENGINE  (does the work in the background)            |
|     finds payments, decides what to invest, places       |
|     orders, handles withdrawals, sends notices           |
+----------------------------------------------------------+
|  4. SIGNER  (guards the keys)                            |
|     creates and locks each user's key, checks every      |
|     request against safety limits, signs approved orders |
+----------------------------------------------------------+
        |                      |                  |
        v                      v                  v
   PostgreSQL          True Markets          ID check vendor
   (all records)       (wallets, stocks)     (Prembly or Dojah)
```

### 1. Front door

- **Telegram webhook (`/webhook`).** Telegram sends every message and button press here. The server checks a shared secret, replies "got it" straight away, and handles the message in the background. Telegram sometimes sends the same message twice, so the server remembers each message ID and ignores repeats.
- **App routes (`/auth`, `/v1`).** Used by the mobile app. Everything under `/v1` needs a signed-in user.
- **Operator routes (`/admin`).** Used by you. They need the `ADMIN_API_KEY` and are switched off if it isn't set.

### 2. Services

- **Sign-in.** Email and password, a one-time code from the bot, or opening the app inside Telegram. Sessions are short, and the app quietly renews them.
- **Sign-up steps.** The user moves through: choose a tenth, get a wallet, verify identity, then they are active. Money features only work once they are active.
- **Identity check.** The NIN and name are checked with a vendor. A close but imperfect name match goes to a person for review. The NIN is stored scrambled, and only until a decision is made.
- **Bot replies.** All the Telegram message texts live here.
- **Confirmation.** Withdrawals and new withdrawal addresses need the password, or a one-time Telegram code that works for that one action only.
- **Rate limits.** Slow down people who guess passwords or request too many codes.

### 3. Engine

A background worker that runs every 30 seconds and does these jobs in order:

1. Finish pending identity checks.
2. Look for new payments in users' wallets.
3. Decide how much to invest from each new payment.
4. Fix orders that got stuck halfway.
5. Place the stock orders.
6. Do the same for withdrawals: fix stuck ones, send them, check they arrived.
7. Check that placed orders settled.
8. Mark each investment as done, partly done or failed.
9. Tell users on Telegram what happened.

If one job fails, the others still run. The to-do list is kept in the database as a "status" on each order, so nothing is lost if the server restarts.

### 4. Signer

Every user has their own secret signing key. The server needs it to approve orders at True Markets.

- **Created once.** The key is made when the user's wallet is set up.
- **Stored locked.** It is locked with a master key from your settings. Only the locked copy is saved. Each locked key only opens for its own user.
- **Unlocked for a split second.** It is opened only while signing.
- **Checked before every signature.** The signer refuses unless: the request hasn't expired, the amount is within the limits, the order document hasn't been changed, and it is an allowed kind of action. Dangerous actions such as exporting keys or changing accounts are always refused.
- **Written down.** Every approval and refusal goes into an audit log.
- **Rotating the master key.** Add a new master key version in settings. New keys use it, and old keys keep working.

## What happens to a payment

```text
payment arrives
   -> server notices it (checked every 30 seconds)
   -> server records it once (never twice)
   -> work out the user's share and split it across their stocks
   -> save the investment and its orders together
   -> for each order: ask True Markets to prepare it
        -> signer checks and signs it
        -> send the signed order
   -> check later that it settled
   -> tell the user
```

A payment is skipped, with the reason recorded, if the user isn't verified yet, the rule is off, the amount is too small, or a daily limit has been reached.

If the server can't tell whether an order went through, for example because it crashed at the wrong moment, it never tries again blindly. It asks True Markets what happened first. An order still unclear after 24 hours is flagged for a person to look at.

## Safety rules

- **Dry run by default.** Until you set `DRY_RUN=false`, orders are planned and recorded, but nothing is sent to True Markets.
- **No double spending.** The database itself refuses a second investment for the same payment, a second order with the same reference, or a Telegram message processed twice.
- **Spending limits.** Separate daily and per-order limits for buying, selling and withdrawing.
- **Money is counted in whole millionths of a dollar.** No rounding errors.
- **Secrets stay secret.** Passwords are scrambled, tokens are stored scrambled, and the NIN is scrambled too.

## Running it

You need Rust, PostgreSQL 17 and a True Markets organization.

```bash
cp .env.example .env              # then fill it in
docker compose up -d usetenth_db  # starts the database
cargo install sqlx-cli --no-default-features --features postgres,rustls
sqlx migrate run --database-url "$POSTGRESQL_DB_URL"
cargo run
```

The server does not set up the database for you, so run the migrations before starting a new version.

Tell Telegram where to send messages. You only do this once:

```bash
curl "https://api.telegram.org/bot$TELEGRAM_BOT_TOKEN/setWebhook" \
  -d url="https://YOUR_PUBLIC_HOST/webhook" \
  -d secret_token="$TELEGRAM_WEBHOOK_SECRET"
```

While developing, set `NGROK_AUTH_TOKEN` and `NGROK_DOMAIN` and the server opens a public tunnel for you and prints the address.

### The settings that matter most

| Setting | What it does |
| --- | --- |
| `APP_MODE` | `development` reads your True Markets key from the file in `TM_KEY_FILE`. `production` fetches it from Turnkey and never falls back to a file. |
| `DRY_RUN` | Leave `true` until you really want to trade. `false` also needs `TM_EXECUTE_AUTH_TYPE`. |
| `SIGNING_MASTER_KEYS` | The master key that locks users' keys, written as `version:64 hex characters`. |
| `SIGNING_ALLOWED_ACTIVITY_TYPES` | The only kinds of action the signer may approve. Left empty, it refuses everything, so set it before trading for real. |
| `KYC_MODE` | `vendor` checks every NIN with the vendor. `auto` approves everyone and only works in development. |
| `ADMIN_API_KEY` | At least 16 characters, or the operator routes stay off. |

Every setting, with its default, is listed in [.env.example](.env.example).

## Routes

All replies look like `{ "success": true, "message": "...", "data": ... }`.

| Area | Routes |
| --- | --- |
| Health | `GET /`, `GET /health` |
| Telegram | `POST /webhook` |
| Accounts | `POST /auth/register`, `/auth/login`, `/auth/refresh`, `/auth/logout`, `/auth/credentials`, `/auth/telegram/login`, `/auth/telegram/webapp`, `/auth/telegram/link`, `GET /auth/me` |
| Setup | `GET /v1/onboarding`, `POST /v1/onboarding/advance`, `/v1/kyc`, `/v1/rule`, `/v1/rule/custom`, `/v1/plans`, `/v1/assets` |
| Money in | `GET /v1/deposit`, `/v1/portfolio`, `/v1/investments` |
| Trading | `POST /v1/trades/quote`, `POST /v1/trades`, `GET /v1/orders`, `/v1/orders/{id}`, `/v1/holdings`, `POST /v1/sells/quote`, `POST /v1/sells` |
| Money out | `POST /v1/confirm`, `GET /v1/withdraw/assets`, `/v1/withdraw/addresses`, `/v1/withdrawals`, and the matching `POST` and `DELETE` routes |
| Operator | `POST /admin/assets/sync`, `PUT /admin/plans/{plan}/allocations`, `GET /admin/orders/review`, `POST /admin/orders/{id}/resolve`, `GET /admin/kyc/review`, `POST /admin/kyc/{id}/resolve` |

## Where things live

```text
src/
  main.rs            starts the server and the background engine
  routes.rs          the list of all routes
  config.rs          reads settings
  handlers/          the front door
  services/          the product rules
  engine/            the background worker
  signing/           the signer
  integrations/      True Markets, key loading, ID check vendors
migrations/          the database layout
```

## Testing

Many tests use a real PostgreSQL database, so start one first:

```bash
docker run -d --name usetenth_test_pg -e POSTGRES_PASSWORD=test -p 5433:5432 postgres:17
export DATABASE_URL=postgres://postgres:test@localhost:5433/postgres
cargo test
```

True Markets, Telegram and the ID check vendors are replaced by fake servers inside the tests, so tests never touch a real service or a real key. The same tests run on every push.
