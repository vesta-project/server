//! In-process game-runtime foundations, not process supervision or authentication.
//!
//! Only bounded console capture is implemented here. No Minecraft process, stdin
//! command dispatch, listener, IPC or readiness/crash-continuity claim is provided.
//! Console output can contain secrets: a future application must enforce current
//! console-read policy before exposing snapshots. Task-view authority is not enough.
#![deny(unsafe_code)]

pub mod console;
