use super::{HashPolicy, KdfCost, MAX_PASSWORD_SCALARS, PasswordHasher, SecretPassword};
use std::{collections::BTreeSet, sync::Arc};

fn fast_hasher() -> PasswordHasher {
    PasswordHasher::new(
        HashPolicy::new(KdfCost::HARD_MIN, KdfCost::DEFAULT_VERIFY_LIMIT)
            .expect("valid low-cost test policy"),
    )
    .expect("construct test hasher")
}

fn password(text: &str) -> SecretPassword {
    SecretPassword::try_from(text.to_owned()).expect("valid test password")
}

fn salt_field(phc: &str) -> &str {
    phc.split('$').nth(4).expect("PHC salt field")
}

#[test]
fn shared_hasher_handles_concurrent_os_salts_and_remains_reusable() {
    const WORKERS: usize = 8;
    const INPUT: &str = "shared concurrent hasher contract";
    let hasher = Arc::new(fast_hasher());
    let verifiers = std::thread::scope(|scope| {
        let workers = (0..WORKERS)
            .map(|_| {
                let shared = Arc::clone(&hasher);
                scope.spawn(move || {
                    shared
                        .hash(&password(INPUT))
                        .expect("concurrent real hash with OS randomness")
                        .as_str()
                        .to_owned()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("hash worker completes"))
            .collect::<Vec<_>>()
    });
    let salts = verifiers
        .iter()
        .map(|phc| salt_field(phc).to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(verifiers.len(), WORKERS);
    assert_eq!(
        salts.len(),
        WORKERS,
        "concurrent calls get distinct OS salts"
    );
    for encoded in &verifiers {
        assert!(
            hasher
                .verify(&password(INPUT), encoded)
                .expect("shared hasher verifies each concurrent result")
        );
    }

    let later = hasher
        .hash(&password(INPUT))
        .expect("reuse shared hasher after concurrent work");
    assert!(!salts.contains(salt_field(later.as_str())));
    assert!(
        hasher
            .verify(&password(INPUT), later.as_str())
            .expect("reused hasher verifies its later result")
    );
}

#[test]
fn nul_and_surrounding_whitespace_are_exact_password_content() {
    let hasher = fast_hasher();
    let exact = password(" \tleft\0right \n");
    let encoded = hasher
        .hash(&exact)
        .expect("hash exact whitespace and NUL bytes");

    assert!(
        hasher
            .verify(&exact, encoded.as_str())
            .expect("verify exact password bytes")
    );
    for mismatch in [
        "left\0right \n",   // removed prefix whitespace
        " \tleft\0right",   // removed suffix whitespace
        " \tleft right \n", // removed embedded NUL
        " \tleft\0right\n", // changed suffix whitespace
    ] {
        assert!(
            !hasher
                .verify(&password(mismatch), encoded.as_str())
                .expect("compare without trimming or prefix normalization")
        );
    }
}

#[test]
fn public_password_errors_redact_input_and_have_no_source_chain() {
    const SECRET: &str = "private-password-error-material";
    // Exceed the scalar bound while remaining below the independent byte bound.
    let rejected = format!("{SECRET}{}", "x".repeat(MAX_PASSWORD_SCALARS));
    let error = SecretPassword::try_from(rejected).expect_err("reject oversized password");
    let rendered = format!("{error} {error:?}");

    assert_eq!(error.code(), "invalid_password");
    assert_eq!(error.to_string(), "Password operation failed");
    assert!(!rendered.contains(SECRET));
    assert!(std::error::Error::source(&error).is_none());
}
