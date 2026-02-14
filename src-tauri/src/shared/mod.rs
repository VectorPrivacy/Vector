//! Shared utilities to eliminate code duplication across modules.
//!
//! This module provides:
//! - `error`: Result extension traits for better error context
//! - `image`: Unified PNG/JPEG encoding functions
//! - `state_access`: Safe STATE lock helper patterns
//!
//! Note: These utilities are set up for future refactoring to eliminate
//! duplicate patterns across the codebase.

#[allow(dead_code)]
pub mod error;
#[allow(dead_code)]
pub mod image;
#[allow(dead_code)]
pub mod state_access;
