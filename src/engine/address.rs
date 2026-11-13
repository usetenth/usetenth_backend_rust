//! Strict withdrawal address checks: a mistyped address loses the money for good.

use sha3::{Digest, Keccak256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Evm,
    Solana,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evm => "evm",
            Self::Solana => "solana",
        }
    }
}

pub fn family_of_chain(chain: &str) -> Family {
    if chain.eq_ignore_ascii_case("solana") { Family::Solana } else { Family::Evm }
}

pub fn detect(address: &str) -> Family {
    if address.starts_with("0x") || address.starts_with("0X") { Family::Evm } else { Family::Solana }
}

pub fn validate(family: Family, address: &str) -> Result<String, &'static str> {
    let address = address.trim();
    match family {
        Family::Evm => validate_evm(address),
        Family::Solana => validate_solana(address),
    }
}

fn validate_evm(address: &str) -> Result<String, &'static str> {
    let hex = address.strip_prefix("0x").or_else(|| address.strip_prefix("0X")).ok_or("EVM addresses start with 0x")?;
    if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("That is not a valid EVM address");
    }
    if hex.bytes().all(|b| b == b'0') {
        return Err("That address cannot receive funds");
    }
    let has_upper = hex.bytes().any(|b| b.is_ascii_uppercase());
    let has_lower = hex.bytes().any(|b| b.is_ascii_lowercase());
    if has_upper && has_lower && checksum(hex) != hex {
        return Err("The address checksum does not match. Check for typos.");
    }
    Ok(format!("0x{}", hex.to_ascii_lowercase()))
}

// EIP-55: a hex letter is uppercase when the matching nibble of keccak256(lowercase hex) is 8 or more.
fn checksum(hex: &str) -> String {
    let lower = hex.to_ascii_lowercase();
    let hash = Keccak256::digest(lower.as_bytes());
    lower
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let nibble = if i % 2 == 0 { hash[i / 2] >> 4 } else { hash[i / 2] & 0x0f };
            if c.is_ascii_alphabetic() && nibble >= 8 { c.to_ascii_uppercase() } else { c }
        })
        .collect()
}

fn validate_solana(address: &str) -> Result<String, &'static str> {
    let bytes = bs58::decode(address).into_vec().map_err(|_| "That is not a valid Solana address")?;
    if bytes.len() != 32 {
        return Err("That is not a valid Solana address");
    }
    if bytes.iter().all(|b| *b == 0) {
        return Err("That address cannot receive funds");
    }
    Ok(address.to_string())
}

/// Addresses compare case-insensitively on EVM (case is only a checksum) and exactly on Solana.
pub fn same_address(family: Family, a: &str, b: &str) -> bool {
    match family {
        Family::Evm => a.eq_ignore_ascii_case(b),
        Family::Solana => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The EIP-55 reference vectors.
    const CHECKSUMMED: [&str; 4] = [
        "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
        "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
        "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
        "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
    ];

    #[test]
    fn accepts_correct_checksums_and_lowercases_the_result() {
        for a in CHECKSUMMED {
            assert_eq!(validate(Family::Evm, a).unwrap(), a.to_ascii_lowercase());
        }
        assert!(validate(Family::Evm, "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed").is_ok(), "all-lowercase has no checksum");
    }

    #[test]
    fn rejects_a_single_wrong_case_character() {
        let bad = CHECKSUMMED[0].replace("aAeb", "AAeb");
        assert!(validate(Family::Evm, &bad).unwrap_err().contains("checksum"));
    }

    #[test]
    fn rejects_malformed_evm_addresses() {
        for bad in ["", "0x", "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed", "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beae", "0xZZaeb6053f3e94c9b9a09f33669435e7ef1beaed", "0x0000000000000000000000000000000000000000"] {
            assert!(validate(Family::Evm, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_solana_addresses() {
        assert!(validate(Family::Solana, "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin").is_ok());
        assert!(validate(Family::Solana, "11111111111111111111111111111111").is_err(), "the all-zero key");
        for bad in ["", "abc", "0xdeadbeef", "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFi0"] {
            assert!(validate(Family::Solana, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn families_follow_the_chain() {
        assert_eq!(family_of_chain("solana"), Family::Solana);
        assert_eq!(family_of_chain("base"), Family::Evm);
        assert_eq!(detect("0xabc"), Family::Evm);
        assert_eq!(detect("9xQe"), Family::Solana);
        assert!(same_address(Family::Evm, "0xAbC", "0xaBc"));
        assert!(!same_address(Family::Solana, "AbC", "aBc"));
    }
}
