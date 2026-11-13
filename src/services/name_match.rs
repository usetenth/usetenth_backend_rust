//! Compares the name on a NIN record with the name a user gave, token by token and typo-tolerant.

use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchResult {
    Pass,
    Review,
    Fail,
}

/// Uppercase, strip accents and punctuation, and drop single-letter tokens (middle initials).
pub fn tokens(value: &str) -> Vec<String> {
    value
        .to_uppercase()
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .map(|c| if c.is_ascii_uppercase() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|t| t.len() > 1)
        .map(str::to_string)
        .collect()
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            let next = (row[j + 1] + 1).min(row[j] + 1).min(prev + cost);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

// One typo is fine, and so is a long name cut short after four letters.
fn token_match(a: &str, b: &str) -> bool {
    levenshtein(a, b) <= 1 || (a.len() >= 4 && b.starts_with(&a[..4]))
}

/// A surname hit plus one more name passes, any overlap goes to review, none fails.
pub fn match_nin_to_name(first: &str, middle: &str, last: &str, given: &str) -> (MatchResult, usize) {
    let nin_tokens = tokens(&format!("{first} {middle} {last}"));
    let given_tokens = tokens(given);
    let surname = tokens(last).into_iter().next().unwrap_or_default();

    let hits = nin_tokens.iter().filter(|n| given_tokens.iter().any(|g| token_match(n, g))).count();
    let surname_hit = !surname.is_empty() && given_tokens.iter().any(|g| levenshtein(&surname, g) <= 1);

    let result = if surname_hit && hits >= 2 {
        MatchResult::Pass
    } else if hits >= 1 {
        MatchResult::Review
    } else {
        MatchResult::Fail
    };
    (result, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_drop_accents_punctuation_and_initials() {
        assert_eq!(tokens("  Ada-Eze J. Obi "), vec!["ADA", "EZE", "OBI"]);
        assert_eq!(tokens("Chidí Nwóke"), vec!["CHIDI", "NWOKE"]);
        assert!(tokens("A B").is_empty());
    }

    #[test]
    fn edit_distance() {
        assert_eq!(levenshtein("KITTEN", "SITTING"), 3);
        assert_eq!(levenshtein("", "ABC"), 3);
        assert_eq!(levenshtein("OBI", "OBI"), 0);
    }

    #[test]
    fn surname_plus_one_name_passes() {
        assert_eq!(match_nin_to_name("Adaeze", "Grace", "Obi", "Obi Adaeze"), (MatchResult::Pass, 2));
        assert_eq!(match_nin_to_name("Adaeze", "", "Obi", "adaeze obi grace"), (MatchResult::Pass, 2));
    }

    #[test]
    fn typos_and_cut_short_names_still_match() {
        assert_eq!(match_nin_to_name("Adaeze", "", "Obiora", "Adaeze Obiorah").0, MatchResult::Pass);
        assert_eq!(match_nin_to_name("Oluwaseun", "", "Adeyemi", "Oluwase Adeyemi").0, MatchResult::Pass);
    }

    #[test]
    fn partial_overlap_goes_to_review_and_none_fails() {
        assert_eq!(match_nin_to_name("Adaeze", "", "Obi", "Adaeze Eze").0, MatchResult::Review);
        assert_eq!(match_nin_to_name("Adaeze", "", "Obi", "Musa Bello").0, MatchResult::Fail);
        assert_eq!(match_nin_to_name("Adaeze", "", "Obi", "").0, MatchResult::Fail);
    }
}
