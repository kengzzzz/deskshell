//! Where the update check gets its list.

use std::future::Future;
use std::pin::Pin;

use crate::model::Package;

pub type UpdateRun = Pin<Box<dyn Future<Output = Result<Checked, String>> + Send>>;

pub trait UpdateSource: Send + Sync {
    /// The pending updates, official first. The service sets `reboot` itself.
    fn check(&self) -> UpdateRun;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    pub packages: Vec<Package>,
    pub warning: Option<String>,
}

impl From<Vec<Package>> for Checked {
    fn from(packages: Vec<Package>) -> Self {
        Self { packages, warning: None }
    }
}
