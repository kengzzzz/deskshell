//! The logind session the agent registers for.

use tracing::debug;
use zbus::proxy::CacheProperties;
use zbus::{Connection, OwnedObjectPath};

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    #[zbus(name = "GetSessionByPID")]
    async fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
    #[zbus(name = "GetUserByPID")]
    async fn get_user_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Session {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.User",
    default_service = "org.freedesktop.login1"
)]
trait User {
    #[zbus(property)]
    fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
}

// The process's session, else its user's display session, as libpolkit-agent-1 and polkitd
// both pick it. A user service runs outside any session.
pub(crate) async fn find(conn: &Connection, pid: u32) -> Result<String, String> {
    let manager = ManagerProxy::builder(conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|error| error.to_string())?;
    match manager.get_session_by_pid(pid).await {
        Ok(path) => {
            let session = SessionProxy::builder(conn)
                .path(path)
                .cache_properties(CacheProperties::No)
                .build()
                .await
                .map_err(|error| error.to_string())?;
            return session
                .id()
                .await
                .map_err(|error| error.to_string());
        }
        Err(error) => debug!(%error, pid, "no session for the process, trying the display session"),
    }
    let path = manager
        .get_user_by_pid(pid)
        .await
        .map_err(|error| format!("No session for pid {pid}: {error}"))?;
    let user = UserProxy::builder(conn)
        .path(path)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|error| error.to_string())?;
    let (id, _) = user
        .display()
        .await
        .map_err(|error| error.to_string())?;
    if id.is_empty() {
        return Err(format!(
            "No session for pid {pid}: the user has no display session"
        ));
    }
    Ok(id)
}
