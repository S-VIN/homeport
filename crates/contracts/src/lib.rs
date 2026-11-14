//! Contracts of capabilities that more than one plugin could implement.
//!
//! A contract holds only the types, ops, events and hooks that other plugins
//! use: no logic and no storage. A plugin implements it and declares
//! `provides: &[health::CAPABILITY]`; users of the capability depend on this
//! crate and write `requires: &[health::CAPABILITY]`, never on the
//! implementation. Add a contract only for a capability that really has (or
//! will have) a second implementation.
pub mod health;
