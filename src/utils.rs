pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn usd(amount: &str) -> String {
    match amount.strip_prefix('-') {
        Some(rest) => format!("-${rest}"),
        None => format!("${amount}"),
    }
}
