mod error;

#[cfg(windows)]
mod win;

pub use error::FiberError;

#[cfg(windows)]
pub use win::Fiber;
