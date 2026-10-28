//! Concrete outbound adapters. Local `SQLite` artifact storage and ZIP source
//! archives are implemented; the other families remain planned.
//! Worker processes may use a different language through a versioned contract.

pub mod archive;
pub mod build;
pub mod export;
pub mod models;
pub mod storage;
pub mod tools;
