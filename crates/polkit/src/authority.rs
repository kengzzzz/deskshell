//! polkitd's `org.freedesktop.PolicyKit1.Authority`, the two methods an agent calls.

use std::collections::HashMap;

use zbus::proxy::CacheProperties;
use zbus::{Connection, Variant};

pub(crate) const BUS_NAME: &str = "org.freedesktop.PolicyKit1";

type Subject<'a> = (&'a str, HashMap<&'a str, Variant<'a>>);

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    async fn register_authentication_agent_with_options(
        &self,
        subject: &Subject<'_>,
        locale: &str,
        object_path: &str,
        options: &HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<()>;

    async fn unregister_authentication_agent(
        &self,
        subject: &Subject<'_>,
        object_path: &str,
    ) -> zbus::Result<()>;
}

#[derive(Debug, Clone)]
pub(crate) struct Registration {
    pub session_id: String,
    pub locale: String,
    pub object_path: String,
}

impl Registration {
    fn subject(&self) -> Subject<'_> {
        (
            "unix-session",
            HashMap::from([(
                "session-id",
                Variant::from(self.session_id.as_str()),
            )]),
        )
    }

    // Calls polkitd's unique name, so the registration is held by the polkitd the agent tracks
    // even if the well-known name moves meanwhile.
    async fn proxy(conn: &Connection, owner: &str) -> zbus::Result<AuthorityProxy<'static>> {
        AuthorityProxy::builder(conn)
            .destination(owner.to_owned())
            .cache_properties(CacheProperties::No)
            .build()
            .await
    }

    pub(crate) async fn register(&self, conn: &Connection, owner: &str) -> zbus::Result<()> {
        Self::proxy(conn, owner)
            .await?
            .register_authentication_agent_with_options(
                &self.subject(),
                &self.locale,
                &self.object_path,
                &HashMap::new(),
            )
            .await
    }

    pub(crate) async fn unregister(&self, conn: &Connection, owner: &str) -> zbus::Result<()> {
        Self::proxy(conn, owner)
            .await?
            .unregister_authentication_agent(&self.subject(), &self.object_path)
            .await
    }
}

// The locale libpolkit-agent-1's `server_register` sends.
pub(crate) fn locale(var: impl Fn(&str) -> Option<String>) -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(var)
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(locale(env(&[])), "en_US.UTF-8");
        assert_eq!(
            locale(env(&[("LANG", "th_TH.UTF-8")])),
            "th_TH.UTF-8"
        );
        assert_eq!(
            locale(env(&[
                ("LANG", "th_TH.UTF-8"),
                ("LC_MESSAGES", "de_DE.UTF-8")
            ])),
            "de_DE.UTF-8"
        );
        assert_eq!(
            locale(env(&[
                ("LC_ALL", ""),
                ("LC_MESSAGES", ""),
                ("LANG", "C.UTF-8")
            ])),
            "C.UTF-8"
        );
        assert_eq!(
            locale(env(&[("LC_ALL", "C"), ("LANG", "C.UTF-8")])),
            "C"
        );
    }
}
