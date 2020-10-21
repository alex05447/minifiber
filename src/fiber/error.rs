use std::{
    error::Error,
    fmt::{Display, Formatter},
    io,
};

#[derive(Debug)]
pub enum FiberError {
    InvalidStackSize,
    ThreadAlreadyAFiber,
    FailedToCreate(io::Error),
}

impl Error for FiberError {}

impl Display for FiberError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use FiberError::*;

        match self {
            InvalidStackSize => "invalid fiber stack size".fmt(f),
            ThreadAlreadyAFiber => "attempted to convert a thread to a fiber more than once".fmt(f),
            FailedToCreate(err) => write!(f, "failed to create the fiber: {}", err),
        }
    }
}
