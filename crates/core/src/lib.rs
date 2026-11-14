//! XkyHold backup engine.
//!
//! Implements the repository format described in `docs/FORMAT.md` of the
//! XkyHold repository. Nothing here knows about the UI or Tauri; storage
//! is reached only through [`destination::Destination`].

pub mod backup;
pub mod bytes;
pub mod crypto;
pub mod destination;
pub mod error;
pub mod pack;
pub mod repo;
pub mod restore;
pub mod rules;
pub mod snapshot;
pub mod tree;

pub use error::{Error, Result};
