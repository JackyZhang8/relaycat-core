use thiserror::Error;

pub type ConnId = u64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoomError {
    #[error("room already has a {role} connection")]
    DuplicateRole { role: &'static str },
}
