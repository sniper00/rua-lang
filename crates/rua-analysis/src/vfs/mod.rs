//! In-memory files, source roots, workspace configuration, and changes.
//!
//! This module does not perform filesystem IO. Loaders and protocol adapters
//! translate external state into explicit changes at the crate boundary.
