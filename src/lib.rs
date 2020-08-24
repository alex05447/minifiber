//! # minifiber
//!
//! Thin Rust wrapper around the Windows fiber API.
//!
//! Technically provides a portable API, but implemented only for Windows at the moment.
//!
//! See [`fibers`](https://docs.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createfiberex) on MSDN for the description of the concept.
//!
//! Run `cargo --doc` for documentation.
//!
//! Uses [`winapi`](https://docs.rs/winapi/*/winapi/).

mod fiber;

pub use fiber::{Fiber, FiberEntryPoint, FiberEntryPointFn, FiberError};
