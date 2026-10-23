pub mod acl;
pub mod framing;
pub mod ids;
pub mod messages;
pub mod notification_state;
pub mod palette;
pub mod png_guard;
pub mod runtime_state;
pub mod sync;
pub mod text;
pub mod transport;
pub mod validate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidIdentity,
    InvalidFrame,
    BufferLimit,
    Flood,
    InvalidOffset,
    InvalidAcl,
    InvalidImage,
    ImageLimit,
    InvalidColor,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentity => "invalid identity",
            Self::InvalidFrame => "invalid bridge frame",
            Self::BufferLimit => "bridge buffer limit exceeded",
            Self::Flood => "bridge rate limit exceeded",
            Self::InvalidOffset => "invalid UTF-16 offset",
            Self::InvalidAcl => "invalid POSIX ACL",
            Self::InvalidImage => "invalid PNG image",
            Self::ImageLimit => "PNG resource limit exceeded",
            Self::InvalidColor => "invalid RGB color",
        })
    }
}

impl std::error::Error for Error {}
