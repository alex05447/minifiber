use {
    std::{fmt::{Display, Formatter}, error::Error, io}
};

#[derive(Debug)]
pub enum FiberError {
    InvalidStackSize,
    FailedToCreate(io::Error),
}

impl Error for FiberError {}

impl Display for FiberError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use FiberError::*;

        match self {
            InvalidStackSize => "invalid fiber stack size".fmt(f),
            FailedToCreate(err) => write!(f, "failed to create the fiber: {}", err),
        }
    }
}