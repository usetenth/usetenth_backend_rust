// Escapes text for Telegram's HTML parse mode.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// Formats a decimal string such as "12.5" or "-3" as dollars.
pub fn usd(amount: &str) -> String {
    match amount.strip_prefix('-') {
        Some(rest) => format!("-${rest}"),
        None => format!("${amount}"),
    }
}
