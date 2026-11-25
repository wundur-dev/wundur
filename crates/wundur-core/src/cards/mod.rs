//! Card manifests: a workflow's card described by the service and drawn by this app, so a new
//! workflow needs no new download (see `docs/card-manifests.md`).
//!
//! The service says what the card holds and how it's laid out, from components this app has
//! ([`manifest::COMPONENTS`]); this app draws them and owns how each behaves. A manifest that
//! uses a component this app lacks isn't offered ([`Manifest::supported`]).
//!
//! This module is the part with no window: reading and checking a manifest ([`manifest`]),
//! what it says about its data ([`expr`]), and a card's data and actions ([`state`]). The
//! `wundur-cards` crate draws it.

pub mod expr;
pub mod manifest;
pub mod remote;
pub mod state;

pub use expr::Scope;
pub use manifest::{Manifest, Node, Problem, Step};
pub use state::{CardState, Effect, Failure, Locals, Request};
