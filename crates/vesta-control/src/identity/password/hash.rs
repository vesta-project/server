use super::{PasswordError, PasswordVerifier, SecretPassword};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const MAX_ENCODED_LENGTH: usize = 256;
const SALT_BYTES: usize = 16;
const HASH_BYTES: usize = 32;
const MIN_FIELD_BYTES: usize = 16;
const MAX_FIELD_BYTES: usize = 64;
const MAX_FIELD_ENCODED: usize = 86;
// Base64's decoded-length estimate rounds up to complete three-byte groups.
// This fixed buffer also accommodates that estimate for a 64-byte field.
const DECODE_BUFFER_BYTES: usize = 66;

/// Memory is in KiB, iterations are passes, parallelism is Argon2 lanes. These
/// application bounds are deliberately much tighter than Argon2's mathematical limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KdfCost {
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl KdfCost {
    /// Test-speed floor, not a recommended production cost.
    pub const HARD_MIN: Self = Self {
        memory_kib: 1024,
        iterations: 1,
        parallelism: 1,
    };
    pub const HARD_MAX: Self = Self {
        memory_kib: 262_144,
        iterations: 16,
        parallelism: 8,
    };
    pub const DEFAULT: Self = Self {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    };
    pub const DEFAULT_VERIFY_LIMIT: Self = Self {
        memory_kib: 65_536,
        iterations: 4,
        parallelism: 4,
    };

    pub fn new(memory_kib: u32, iterations: u32, parallelism: u32) -> Result<Self, PasswordError> {
        if !(1024..=262_144).contains(&memory_kib)
            || !(1..=16).contains(&iterations)
            || !(1..=8).contains(&parallelism)
            || memory_kib < 8 * parallelism
        {
            return Err(PasswordError::InvalidPolicy);
        }
        Ok(Self {
            memory_kib,
            iterations,
            parallelism,
        })
    }

    pub const fn memory_kib(self) -> u32 {
        self.memory_kib
    }

    pub const fn iterations(self) -> u32 {
        self.iterations
    }

    pub const fn parallelism(self) -> u32 {
        self.parallelism
    }

    fn within(self, limit: Self) -> bool {
        self.memory_kib <= limit.memory_kib
            && self.iterations <= limit.iterations
            && self.parallelism <= limit.parallelism
    }

    fn at_least(self, other: Self) -> bool {
        other.within(self)
    }
}

impl Default for KdfCost {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HashPolicy {
    cost: KdfCost,
    verify_limit: KdfCost,
}

impl HashPolicy {
    pub fn new(cost: KdfCost, verify_limit: KdfCost) -> Result<Self, PasswordError> {
        if !cost.within(verify_limit) {
            return Err(PasswordError::InvalidPolicy);
        }
        Ok(Self { cost, verify_limit })
    }

    pub const fn cost(self) -> KdfCost {
        self.cost
    }

    pub const fn verify_limit(self) -> KdfCost {
        self.verify_limit
    }
}

impl Default for HashPolicy {
    fn default() -> Self {
        Self {
            cost: KdfCost::DEFAULT,
            verify_limit: KdfCost::DEFAULT_VERIFY_LIMIT,
        }
    }
}

/// Immutable primitive. Admission, authorization, persistence and audit belong to
/// the application. No high-level verifier can import unchecked PHC costs here.
#[derive(Debug, Default)]
pub struct PasswordHasher {
    policy: HashPolicy,
}

impl PasswordHasher {
    pub fn new(policy: HashPolicy) -> Result<Self, PasswordError> {
        let policy = HashPolicy::new(policy.cost, policy.verify_limit)?;
        Ok(Self { policy })
    }

    pub const fn policy(&self) -> HashPolicy {
        self.policy
    }

    pub fn hash(&self, password: &SecretPassword) -> Result<PasswordVerifier, PasswordError> {
        let mut salt = Zeroizing::new([0_u8; SALT_BYTES]);
        getrandom::fill(&mut *salt).map_err(|_| PasswordError::RandomUnavailable)?;
        let mut derived = Zeroizing::new([0_u8; HASH_BYTES]);
        derive(password, &salt[..], self.policy.cost, &mut derived[..])?;
        // Encoding creates extra owned copies of verifier material. Guard those
        // too, not just the raw bytes and the final persistence value.
        let encoded_salt = Zeroizing::new(STANDARD_NO_PAD.encode(&salt[..]));
        let encoded_hash = Zeroizing::new(STANDARD_NO_PAD.encode(&derived[..]));
        let cost = self.policy.cost;
        let phc = format!(
            "$argon2id$v=19$m={},t={},p={}${}${}",
            cost.memory_kib,
            cost.iterations,
            cost.parallelism,
            encoded_salt.as_str(),
            encoded_hash.as_str()
        );
        Ok(PasswordVerifier::new(phc))
    }

    /// False means ONLY a well-formed, bounded verifier whose output does not match.
    /// All PHC and cost validation precedes any cost-dependent allocation or derivation.
    pub fn verify(&self, password: &SecretPassword, encoded: &str) -> Result<bool, PasswordError> {
        let parsed = parse(encoded, self.policy.verify_limit)?;
        let mut derived = Zeroizing::new([0_u8; MAX_FIELD_BYTES]);
        let output = &mut derived[..parsed.hash_len];
        derive(
            password,
            &parsed.salt[..parsed.salt_len],
            parsed.cost,
            output,
        )?;
        // Both slices have the exact validated output length. Subtle's slice
        // comparison is constant-time in contents (not in public slice length).
        Ok(bool::from(output.ct_eq(&parsed.hash[..parsed.hash_len])))
    }

    /// Observation only: does not derive, mutate or automatically replace a hash.
    /// An entirely stronger cost is not marked for replacement solely for differing
    /// from the new-hash cost; mismatched salt/output sizes still need replacement.
    pub fn needs_rehash(&self, encoded: &str) -> Result<bool, PasswordError> {
        let parsed = parse(encoded, self.policy.verify_limit)?;
        Ok(!parsed.cost.at_least(self.policy.cost)
            || parsed.salt_len != SALT_BYTES
            || parsed.hash_len != HASH_BYTES)
    }
}

struct Parsed {
    cost: KdfCost,
    salt: Zeroizing<[u8; DECODE_BUFFER_BYTES]>,
    salt_len: usize,
    hash: Zeroizing<[u8; DECODE_BUFFER_BYTES]>,
    hash_len: usize,
}

/// A bounded format envelope, NOT a cryptographic implementation. Parameter order
/// is immaterial; all three names must occur exactly once. Emission is canonical m,t,p.
/// We intentionally retain 16..64 decoded bytes rather than the PHC crate's narrower
/// salt bound. Decimal and Base64 work use standard/library APIs and fixed buffers.
fn parse(encoded: &str, limit: KdfCost) -> Result<Parsed, PasswordError> {
    if encoded.is_empty()
        || encoded.len() > MAX_ENCODED_LENGTH
        || !encoded.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(PasswordError::MalformedVerifier);
    }
    let mut fields = encoded.split('$');
    let malformed = || PasswordError::MalformedVerifier;
    if fields.next() != Some("") {
        return Err(malformed());
    }
    let algorithm = fields.next().ok_or_else(malformed)?;
    let version = fields.next().ok_or_else(malformed)?;
    let parameters = fields.next().ok_or_else(malformed)?;
    let salt = fields.next().ok_or_else(malformed)?;
    let hash = fields.next().ok_or_else(malformed)?;
    if fields.next().is_some() {
        return Err(malformed());
    }
    if algorithm != "argon2id" || version != "v=19" {
        return Err(PasswordError::UnsupportedVerifier);
    }

    let mut costs = [0_u32; 3];
    let mut seen = 0_u8;
    for parameter in parameters.split(',') {
        let (name, value) = parameter.split_once('=').ok_or_else(malformed)?;
        let index = match name {
            "m" => 0,
            "t" => 1,
            "p" => 2,
            _ => return Err(malformed()),
        };
        let bit = 1_u8 << index;
        if seen & bit != 0 {
            return Err(malformed());
        }
        costs[index] = decimal(value)?;
        seen |= bit;
    }
    if seen != 0b111 {
        return Err(malformed());
    }
    let cost =
        KdfCost::new(costs[0], costs[1], costs[2]).map_err(|_| PasswordError::CostOutOfBounds)?;
    if !cost.within(limit) {
        return Err(PasswordError::CostOutOfBounds);
    }
    let (salt, salt_len) = decode_field(salt)?;
    let (hash, hash_len) = decode_field(hash)?;
    Ok(Parsed {
        cost,
        salt,
        salt_len,
        hash,
        hash_len,
    })
}

fn decimal(value: &str) -> Result<u32, PasswordError> {
    if value.is_empty()
        || value.len() > 10
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(PasswordError::MalformedVerifier);
    }
    value.parse().map_err(|_| PasswordError::MalformedVerifier)
}

fn decode_field(
    text: &str,
) -> Result<(Zeroizing<[u8; DECODE_BUFFER_BYTES]>, usize), PasswordError> {
    // An unpadded 16-byte field needs 22 characters; 64 bytes need at most 86.
    if !(22..=MAX_FIELD_ENCODED).contains(&text.len()) {
        return Err(PasswordError::MalformedVerifier);
    }
    let mut decoded = Zeroizing::new([0_u8; DECODE_BUFFER_BYTES]);
    let len = STANDARD_NO_PAD
        .decode_slice(text, &mut decoded[..])
        .map_err(|_| PasswordError::MalformedVerifier)?;
    if !(MIN_FIELD_BYTES..=MAX_FIELD_BYTES).contains(&len) {
        return Err(PasswordError::MalformedVerifier);
    }
    // Explicit canonical round-trip also rejects padding and non-zero trailing bits,
    // without allocating an attacker-sized decoded buffer or encoded String.
    let mut canonical = Zeroizing::new([0_u8; MAX_FIELD_ENCODED]);
    let encoded_len = STANDARD_NO_PAD
        .encode_slice(&decoded[..len], &mut canonical[..])
        .map_err(|_| PasswordError::MalformedVerifier)?;
    if &canonical[..encoded_len] != text.as_bytes() {
        return Err(PasswordError::MalformedVerifier);
    }
    Ok((decoded, len))
}

fn derive(
    password: &SecretPassword,
    salt: &[u8],
    cost: KdfCost,
    output: &mut [u8],
) -> Result<(), PasswordError> {
    #[cfg(test)]
    tests::DERIVATIONS.with(|count| count.set(count.get() + 1));
    let params = Params::new(
        cost.memory_kib,
        cost.iterations,
        cost.parallelism,
        Some(output.len()),
    )
    .map_err(|_| PasswordError::InvalidPolicy)?;
    let blocks = params.block_count();
    // Argon2's internal allocating Blocks drops without wiping its working heap.
    // Own the supported caller-provided buffer instead, reserve fallibly before
    // initialization, and never grow/reallocate it after derived data is written.
    #[cfg(test)]
    tests::ALLOCATIONS.with(|count| count.set(count.get() + 1));
    let mut memory = Zeroizing::new(Vec::<Block>::new());
    memory
        .try_reserve_exact(blocks)
        .map_err(|_| PasswordError::KdfUnavailable)?;
    memory.resize_with(blocks, Block::default);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(password.as_bytes(), salt, output, &mut memory[..])
        .map_err(|_| PasswordError::KdfUnavailable)
}

#[cfg(test)]
pub(crate) fn test_work_counts() -> (usize, usize) {
    (
        tests::DERIVATIONS.with(std::cell::Cell::get),
        tests::ALLOCATIONS.with(std::cell::Cell::get),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        pub(super) static DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        pub(super) static ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn fast() -> PasswordHasher {
        PasswordHasher::new(
            HashPolicy::new(KdfCost::HARD_MIN, KdfCost::DEFAULT_VERIFY_LIMIT).unwrap(),
        )
        .unwrap()
    }

    fn password(text: &str) -> SecretPassword {
        SecretPassword::try_from(text.to_owned()).unwrap()
    }

    fn shape(parameters: &str, salt_len: usize, hash_len: usize) -> String {
        format!(
            "$argon2id$v=19${parameters}${}${}",
            STANDARD_NO_PAD.encode(vec![0_u8; salt_len]),
            STANDARD_NO_PAD.encode(vec![0_u8; hash_len])
        )
    }

    #[test]
    fn production_cost_fresh_salt_and_exact_output_are_explicit() {
        let hasher = PasswordHasher::default();
        let secret = password("public password fixture");
        let first = hasher.hash(&secret).unwrap();
        let second = hasher.hash(&secret).unwrap();
        assert!(
            first
                .as_str()
                .starts_with("$argon2id$v=19$m=19456,t=2,p=1$")
        );
        assert_ne!(first.as_str(), second.as_str());
        let parsed = parse(first.as_str(), KdfCost::DEFAULT_VERIFY_LIMIT).unwrap();
        assert_eq!(parsed.salt_len, SALT_BYTES);
        assert_eq!(parsed.hash_len, HASH_BYTES);
        assert!(hasher.verify(&secret, first.as_str()).unwrap());
        assert!(
            !hasher
                .verify(&password("different"), first.as_str())
                .unwrap()
        );
        assert!(!hasher.needs_rehash(first.as_str()).unwrap());
        assert!(!format!("{first:?}").contains(first.as_str()));
        assert!(!format!("{hasher:?}").contains(first.as_str()));
    }

    #[test]
    fn independent_bun_ascii_and_utf8_phc_fixtures_match() {
        // Public independent Bun 1.3.14 Argon2id fixtures from PasswordHasherTest.java.
        let ascii = "$argon2id$v=19$m=19456,t=2,p=1$HK+ejNGt8MY161luKZ1r5ih0jPow7v2LLPfMx28uPNk$Z2hzM3WmcE55SafQUHKfAnXQ46iPtGH7+P7si2acXWk";
        let utf8 = "$argon2id$v=19$m=19456,t=2,p=1$xrDkR5kg45KHoAyegBqrAQJKrIN92DH9mPgEdQSwR1c$z0w/VLz8qh8B+B5iuiX9lHUnhXNZt/Alw8+Z20wVwds";
        let hasher = PasswordHasher::default();
        assert!(
            hasher
                .verify(&password("vesta-public-test-password"), ascii)
                .unwrap()
        );
        assert!(
            hasher
                .verify(&password("Vesta-\u{1f510}-café"), utf8)
                .unwrap()
        );
        assert!(
            !hasher
                .verify(&password("vesta-public-test-password!"), ascii)
                .unwrap()
        );
        assert!(hasher.needs_rehash(ascii).unwrap());
    }

    #[test]
    fn all_parameter_permutations_are_equivalent_and_emission_is_ordered() {
        for parameters in [
            "m=1024,t=1,p=1",
            "m=1024,p=1,t=1",
            "t=1,m=1024,p=1",
            "t=1,p=1,m=1024",
            "p=1,m=1024,t=1",
            "p=1,t=1,m=1024",
        ] {
            let parsed = parse(&shape(parameters, 16, 32), KdfCost::HARD_MIN).unwrap();
            assert_eq!(parsed.cost, KdfCost::HARD_MIN);
        }
        let hasher = fast();
        let encoded = hasher.hash(&password("fixture")).unwrap();
        let permuted = encoded.as_str().replace("m=1024,t=1,p=1", "p=1,t=1,m=1024");
        assert!(hasher.verify(&password("fixture"), &permuted).unwrap());
    }

    #[test]
    fn decoded_salt_and_hash_limits_are_inclusive_through_64_bytes() {
        let secret = password("public long-field fixture");
        for salt_len in [16, 48, 49, 64] {
            for hash_len in [16, 32, 64] {
                let salt = vec![0x5a_u8; salt_len];
                let mut output = Zeroizing::new(vec![0_u8; hash_len]);
                derive(&secret, &salt, KdfCost::HARD_MIN, &mut output[..]).unwrap();
                let encoded = format!(
                    "$argon2id$v=19$m=1024,t=1,p=1${}${}",
                    STANDARD_NO_PAD.encode(&salt),
                    STANDARD_NO_PAD.encode(&output[..])
                );
                assert!(fast().verify(&secret, &encoded).unwrap());
            }
        }
    }

    #[test]
    fn unsupported_malformed_and_unbounded_envelopes_fail_before_derivation() {
        DERIVATIONS.with(|count| count.set(0));
        ALLOCATIONS.with(|count| count.set(0));
        let valid = shape("m=1024,t=1,p=1", 16, 32);
        let malformed = [
            "".to_owned(),
            "plaintext public fixture".to_owned(),
            "x".repeat(MAX_ENCODED_LENGTH + 1),
            format!(" {valid}"),
            format!("{valid}\n"),
            format!("{valid}$"),
            valid.replace("m=1024", "m=01024"),
            valid.replace("m=1024", "m=+1024"),
            valid.replace("m=1024", "m=-1024"),
            valid.replace("m=1024", "m=4294967296"),
            valid.replace("m=1024", "m=1e6"),
            valid.replace("m=1024,t=1,p=1", "m=1024,t=1,t=1"),
            valid.replace("m=1024,t=1,p=1", "m=1024,t=1"),
            valid.replace("p=1", "p=1,keyid=AA"),
            valid.replace("p=1", "p=1,data=AA"),
            valid.replace("p=1", "p=1,x=2"),
            valid.replace("t=1", "t=1=1"),
            shape("m=1024,t=1,p=1", 15, 32),
            shape("m=1024,t=1,p=1", 65, 32),
            shape("m=1024,t=1,p=1", 16, 15),
            shape("m=1024,t=1,p=1", 16, 65),
        ];
        for encoded in malformed {
            assert_eq!(
                fast().verify(&password("fixture"), &encoded).unwrap_err(),
                PasswordError::MalformedVerifier
            );
            assert!(fast().needs_rehash(&encoded).is_err());
        }
        for encoded in [
            valid.replace("argon2id", "argon2i"),
            valid.replace("v=19", "v=16"),
            valid.replace("v=19", "v=019"),
        ] {
            assert_eq!(
                fast().verify(&password("fixture"), &encoded).unwrap_err(),
                PasswordError::UnsupportedVerifier
            );
        }
        for parameters in [
            "m=0,t=1,p=1",
            "m=1023,t=1,p=1",
            "m=65537,t=1,p=1",
            "m=262145,t=1,p=1",
            "m=4294967295,t=1,p=1",
            "m=1024,t=0,p=1",
            "m=1024,t=5,p=1",
            "m=1024,t=17,p=1",
            "m=1024,t=1,p=0",
            "m=1024,t=1,p=5",
            "m=1024,t=1,p=9",
        ] {
            assert_eq!(
                fast()
                    .verify(&password("fixture"), &shape(parameters, 16, 32))
                    .unwrap_err(),
                PasswordError::CostOutOfBounds
            );
        }
        DERIVATIONS.with(|count| assert_eq!(count.get(), 0, "rejections never derive"));
        ALLOCATIONS.with(|count| assert_eq!(count.get(), 0, "rejections never reserve KDF memory"));
    }

    #[test]
    fn canonical_base64_rejects_padding_alphabet_and_nonzero_unused_bits() {
        let canonical = STANDARD_NO_PAD.encode([0_u8; 16]);
        let altered = format!("{}B", &canonical[..canonical.len() - 1]);
        for text in [
            format!("{canonical}="),
            altered,
            canonical.replace('A', "_"),
        ] {
            assert_eq!(
                decode_field(&text).unwrap_err(),
                PasswordError::MalformedVerifier
            );
        }
    }

    #[test]
    fn policies_have_hard_bounds_and_rehash_never_demands_a_cost_downgrade() {
        for (m, t, p) in [
            (1023, 1, 1),
            (262145, 1, 1),
            (1024, 0, 1),
            (1024, 17, 1),
            (1024, 1, 0),
            (1024, 1, 9),
            (1024, 1, u32::MAX),
        ] {
            assert_eq!(
                KdfCost::new(m, t, p).unwrap_err(),
                PasswordError::InvalidPolicy
            );
        }
        assert_eq!(
            HashPolicy::new(KdfCost::DEFAULT, KdfCost::HARD_MIN).unwrap_err(),
            PasswordError::InvalidPolicy
        );
        let production = PasswordHasher::default();
        assert!(
            !production
                .needs_rehash(&shape("m=32768,t=3,p=2", 16, 32))
                .unwrap()
        );
        assert!(
            production
                .needs_rehash(&shape("m=32768,t=1,p=1", 16, 32))
                .unwrap()
        );
        assert!(
            production
                .needs_rehash(&shape("m=32768,t=3,p=2", 32, 32))
                .unwrap()
        );
        assert!(
            production
                .needs_rehash(&shape("m=32768,t=3,p=2", 16, 16))
                .unwrap()
        );
    }

    #[test]
    fn unicode_passwords_are_not_normalized_or_truncated() {
        let hasher = fast();
        let maximum = password(&"\u{1f510}".repeat(1024));
        let hash = hasher.hash(&maximum).unwrap();
        assert!(hasher.verify(&maximum, hash.as_str()).unwrap());
        let composed = hasher.hash(&password("café")).unwrap();
        assert!(
            !hasher
                .verify(&password("cafe\u{301}"), composed.as_str())
                .unwrap()
        );
    }
}
