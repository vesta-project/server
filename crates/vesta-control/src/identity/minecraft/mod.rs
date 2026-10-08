//! Bounded online Minecraft profile verification, not Vesta caller authentication.
//!
//! A verified profile is an observation of Mojang's join result. It is never a
//! `Subject`, device credential, session, pack grant, local proof or management
//! authority. No canonical identity is linked or persisted here. Production
//! handles share process-wide admission; test injection exists only in unit tests.
mod challenge;
mod provider;
mod types;

pub use challenge::MinecraftChallenges;
pub use types::{
    ChallengeId, IssuedChallenge, MinecraftError, MinecraftErrorCode, MinecraftName, MinecraftUuid,
    ServerHash, ServerUuid, VerifiedMinecraftProfile,
};

#[cfg(test)]
use challenge::{Clock, TestLimits};
#[cfg(test)]
use provider::{JoinedProfile, SessionProvider};

#[cfg(test)]
mod tests;
