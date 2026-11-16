//! One-way bridges from the desktop user to a second user, `tenant`, in the same Wayland session.

pub mod cli;
pub mod frame;
pub mod launch;
pub mod notify;
pub mod shortcuts;
pub mod tray;
pub mod wayland;

#[cfg(test)]
mod test_dir;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bus {
    Session,
    Address(String),
}

impl Bus {
    fn builder(&self) -> zbus::connection::Builder<'static> {
        match self {
            Self::Session => zbus::connection::Builder::session(),
            Self::Address(address) => zbus::connection::Builder::address(address.as_str()),
        }
    }
}
