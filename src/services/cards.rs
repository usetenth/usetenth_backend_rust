//! Picture cards shown above bot messages, rendered ahead of time from the brand design.

const WELCOME: &[u8] = include_bytes!("../../assets/cards/welcome.png");

macro_rules! tenth_cards {
    ($($percent:literal => [$steady:literal, $growth:literal, $tech:literal, $custom:literal]),*) => {
        fn tenth_card(percent: i16, mix: &str) -> Option<&'static [u8]> {
            match (percent, mix) {
                $(
                    ($percent, "Steady") => Some(include_bytes!(concat!("../../assets/cards/tenth-", $steady, ".png"))),
                    ($percent, "Growth") => Some(include_bytes!(concat!("../../assets/cards/tenth-", $growth, ".png"))),
                    ($percent, "Tech-heavy") => Some(include_bytes!(concat!("../../assets/cards/tenth-", $tech, ".png"))),
                    ($percent, "Your picks") => Some(include_bytes!(concat!("../../assets/cards/tenth-", $custom, ".png"))),
                )*
                _ => None,
            }
        }
    };
}

tenth_cards!(
    5 => ["5-steady", "5-growth", "5-tech", "5-custom"],
    10 => ["10-steady", "10-growth", "10-tech", "10-custom"],
    20 => ["20-steady", "20-growth", "20-tech", "20-custom"]
);

pub fn welcome() -> &'static [u8] {
    WELCOME
}

/// The card for a percentage and mix name; None for a percentage the app does not offer.
pub fn tenth(percent: i16, mix_name: &str) -> Option<&'static [u8]> {
    tenth_card(percent, mix_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G'];

    #[test]
    fn every_offered_percentage_and_mix_has_a_picture() {
        assert!(welcome().starts_with(PNG));
        for percent in [5, 10, 20] {
            for mix in ["Steady", "Growth", "Tech-heavy", "Your picks"] {
                assert!(tenth(percent, mix).is_some_and(|png| png.starts_with(PNG)), "{percent} {mix}");
            }
        }
    }

    #[test]
    fn the_cards_differ_by_percentage_and_mix() {
        assert_ne!(tenth(5, "Steady"), tenth(10, "Steady"));
        assert_ne!(tenth(10, "Steady"), tenth(10, "Growth"));
    }

    #[test]
    fn unknown_percentages_and_mixes_have_no_card() {
        assert!(tenth(15, "Steady").is_none());
        assert!(tenth(10, "Mystery").is_none());
    }
}
