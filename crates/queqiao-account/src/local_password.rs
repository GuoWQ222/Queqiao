//! A password kept on this PC only (the settings password): stored as PBKDF2-HMAC-SHA256 with a random salt,
//! "pbkdf2-sha256$<rounds>$<salt hex>$<hash hex>".

use std::num::NonZeroU32;

use ring::pbkdf2::{PBKDF2_HMAC_SHA256, derive, verify};

const ROUNDS: u32 = 200_000;

/// The stored form of `password`.
pub fn hash_local_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("the system random number generator");
    let rounds = NonZeroU32::new(ROUNDS).expect("not zero");
    let mut hash = [0u8; 32];
    derive(
        PBKDF2_HMAC_SHA256,
        rounds,
        &salt,
        password.as_bytes(),
        &mut hash,
    );
    format!(
        "pbkdf2-sha256${ROUNDS}${}${}",
        crate::config::hex(&salt),
        crate::config::hex(&hash)
    )
}

/// Whether `password` matches `stored` (compared in constant time by ring).
pub fn check_local_password(stored: &str, password: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    let [kind, rounds, salt, hash] = parts.as_slice() else {
        return false;
    };
    let (Some(rounds), Some(salt), Some(hash)) = (
        rounds.parse::<u32>().ok().and_then(NonZeroU32::new),
        unhex(salt),
        unhex(hash),
    ) else {
        return false;
    };
    *kind == "pbkdf2-sha256"
        && verify(
            PBKDF2_HMAC_SHA256,
            rounds,
            &salt,
            password.as_bytes(),
            &hash,
        )
        .is_ok()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_right_password_only() {
        let stored = hash_local_password("correct horse");
        assert!(stored.starts_with("pbkdf2-sha256$200000$"));
        assert!(check_local_password(&stored, "correct horse"));
        assert!(!check_local_password(&stored, "correct hors"));
        assert!(!check_local_password(&stored, ""));
        // Two hashes of one password differ (salt), and garbage is never accepted.
        assert_ne!(stored, hash_local_password("correct horse"));
        assert!(!check_local_password("", "x"));
        assert!(!check_local_password("pbkdf2-sha256$0$00$00", "x"));
        assert!(!check_local_password("pbkdf2-sha256$1$zz$00", "x"));
    }
}
