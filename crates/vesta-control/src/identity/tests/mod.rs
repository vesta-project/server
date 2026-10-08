//! Real-database identity test families.
//!
//! Every module here runs against actual protected SQLite state, real transactions, the
//! real audit chain and the real identity service. There is no fake service, audit shim,
//! injected clock or stubbed authority. `support` is shared scaffolding, not evidence.
#[allow(dead_code)]
mod support;

mod atomicity;
mod bootstrap;
mod delegation;
mod fixture_contract;
mod policy;
mod scopes;
