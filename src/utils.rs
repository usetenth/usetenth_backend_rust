pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn usd(amount: &str) -> String {
    match amount.strip_prefix('-') {
        Some(rest) => format!("-${rest}"),
        None => format!("${amount}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_special_characters_are_escaped() {
        assert_eq!(escape_html("<b>Tom & \"Jerry\"</b>"), "&lt;b&gt;Tom &amp; \"Jerry\"&lt;/b&gt;");
        assert_eq!(escape_html("&lt;"), "&amp;lt;", "already-escaped text is escaped again, never trusted");
        assert_eq!(escape_html("plain"), "plain");
    }

    #[test]
    fn dollar_amounts_keep_their_sign_in_front_of_the_symbol() {
        assert_eq!(usd("12.50"), "$12.50");
        assert_eq!(usd("-3.10"), "-$3.10");
        assert_eq!(usd("0.00"), "$0.00");
    }
}
