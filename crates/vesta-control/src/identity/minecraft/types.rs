use std::{error::Error, fmt, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinecraftErrorCode {
    InvalidRequest,
    Busy,
    RateLimited,
    Unavailable,
}
impl MinecraftErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "minecraft_invalid_request",
            Self::Busy => "minecraft_busy",
            Self::RateLimited => "minecraft_rate_limited",
            Self::Unavailable => "minecraft_unavailable",
        }
    }
}

/// Only fixed codes survive; no native error, provider payload or query is retained.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct MinecraftError(MinecraftErrorCode);
impl MinecraftError {
    pub const fn code(self) -> &'static str {
        self.0.code()
    }
    pub(super) const fn invalid() -> Self {
        Self(MinecraftErrorCode::InvalidRequest)
    }
    pub(super) const fn busy() -> Self {
        Self(MinecraftErrorCode::Busy)
    }
    pub(super) const fn rate_limited() -> Self {
        Self(MinecraftErrorCode::RateLimited)
    }
    pub(super) const fn unavailable() -> Self {
        Self(MinecraftErrorCode::Unavailable)
    }
}
impl fmt::Display for MinecraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Minecraft verification failed")
    }
}
impl fmt::Debug for MinecraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MinecraftError")
            .field("code", &self.code())
            .finish()
    }
}
impl Error for MinecraftError {}

fn parse_uuid(value: &str) -> Result<Uuid, MinecraftError> {
    let valid = match value.len() {
        32 => value.bytes().all(|b| b.is_ascii_hexdigit()),
        36 => value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }),
        _ => false,
    };
    if !valid {
        return Err(MinecraftError::invalid());
    }
    let uuid = Uuid::parse_str(value).map_err(|_| MinecraftError::invalid())?;
    if uuid.is_nil() {
        return Err(MinecraftError::invalid());
    }
    Ok(uuid)
}

macro_rules! uuid_value {
    ($name:ident) => {
        #[derive(Clone, Copy, Eq, Hash, PartialEq)]
        pub struct $name(Uuid);
        impl $name {
            pub fn parse(value: &str) -> Result<Self, MinecraftError> {
                parse_uuid(value).map(Self)
            }
            pub fn from_uuid(value: Uuid) -> Result<Self, MinecraftError> {
                if value.is_nil() {
                    return Err(MinecraftError::invalid());
                }
                Ok(Self(value))
            }
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "[<redacted>]"))
            }
        }
    };
}
uuid_value!(ServerUuid);
uuid_value!(MinecraftUuid);

/// Canonical profile spelling is retained. Matching may only ignore ASCII case.
#[derive(Clone, Eq, PartialEq)]
pub struct MinecraftName(String);
impl MinecraftName {
    pub fn parse(value: &str) -> Result<Self, MinecraftError> {
        if !(1..=16).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(MinecraftError::invalid());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for MinecraftName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MinecraftName[<redacted>]")
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct ChallengeId(String);
impl ChallengeId {
    pub fn parse(value: &str) -> Result<Self, MinecraftError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MinecraftError::invalid());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn from_nonce(nonce: &[u8; 32]) -> Self {
        Self(hex::encode(nonce))
    }
}
impl fmt::Debug for ChallengeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChallengeId[<redacted>]")
    }
}

/// Locally generated Mojang serverId; never an accepted caller claim.
/// There is no public parser or deserializer for caller-selected hashes.
///
/// ```compile_fail
/// use vesta_control::identity::minecraft::ServerHash;
/// let claimed_hash = ServerHash::parse("0123456789012345678901234567890123456789");
/// ```
#[derive(Clone, Eq, PartialEq)]
pub struct ServerHash(String);
impl ServerHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn from_digest(bytes: &[u8; 20]) -> Self {
        Self(hex::encode(bytes))
    }
}
impl fmt::Debug for ServerHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerHash[<redacted>]")
    }
}

pub struct IssuedChallenge {
    pub(super) id: ChallengeId,
    pub(super) hash: ServerHash,
    pub(super) lifetime: Duration,
}
impl IssuedChallenge {
    pub fn challenge_id(&self) -> &ChallengeId {
        &self.id
    }
    pub fn server_hash(&self) -> &ServerHash {
        &self.hash
    }
    /// Relative lifetime starts at insertion, not when a transport receives this value.
    pub const fn lifetime(&self) -> Duration {
        self.lifetime
    }
}
impl fmt::Debug for IssuedChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedChallenge")
            .field("lifetime", &self.lifetime)
            .field("challenge", &"<redacted>")
            .finish()
    }
}

/// Server-bound online-profile observation. No clone, deserializer or public issuer.
/// This is not proof of a device key, an authenticated Vesta Subject or authorization;
/// there is no conversion to any principal, session, role, grant or local proof.
///
/// ```compile_fail
/// use vesta_control::identity::minecraft::{
///     MinecraftName, MinecraftUuid, ServerUuid, VerifiedMinecraftProfile,
/// };
/// let claimed_profile = VerifiedMinecraftProfile {
///     server: ServerUuid::parse("11111111-1111-1111-1111-111111111111").unwrap(),
///     uuid: MinecraftUuid::parse("22222222-2222-2222-2222-222222222222").unwrap(),
///     name: MinecraftName::parse("PlayerOne").unwrap(),
/// };
/// ```
pub struct VerifiedMinecraftProfile {
    pub(super) server: ServerUuid,
    pub(super) uuid: MinecraftUuid,
    pub(super) name: MinecraftName,
}
impl VerifiedMinecraftProfile {
    pub const fn server_uuid(&self) -> ServerUuid {
        self.server
    }
    pub const fn uuid(&self) -> MinecraftUuid {
        self.uuid
    }
    pub fn name(&self) -> &MinecraftName {
        &self.name
    }
}
impl fmt::Debug for VerifiedMinecraftProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedMinecraftProfile[<redacted>]")
    }
}
