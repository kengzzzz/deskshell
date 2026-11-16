//! The identities polkitd offers in `BeginAuthentication`, and the one the agent picks.

use std::collections::HashMap;
use std::ffi::CStr;

use tracing::warn;
use zbus::OwnedVariant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub uid: u32,
    /// `None` when the uid has no passwd entry. The conversation then fails at once.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Offered {
    User(u32),
    Group(u32),
    Netgroup(String),
}

pub(crate) type RawIdentity = (String, HashMap<String, OwnedVariant>);

// Parses and words its errors as `polkit_identity_new_for_gvariant` and the listener do.
pub(crate) fn parse(raw: &[RawIdentity]) -> Result<Vec<Offered>, String> {
    raw.iter()
        .enumerate()
        .map(|(n, (kind, details))| {
            parse_one(kind, details)
                .map_err(|error| format!("Error extracting identity {n}: {error}"))
        })
        .collect()
}

fn parse_one(kind: &str, details: &HashMap<String, OwnedVariant>) -> Result<Offered, String> {
    let id = |key: &str| -> Result<u32, String> {
        let value = details
            .get(key)
            .ok_or_else(|| format!("Didn't find value for key `{key}'"))?;
        u32::try_from(value).map_err(|_| format!("Value for key `{key}' is not of type u"))
    };
    match kind {
        "unix-user" => id("uid")
            .map(Offered::User)
            .map_err(|error| format!("Error parsing unix-user identity: {error}")),
        "unix-group" => id("gid")
            .map(Offered::Group)
            .map_err(|error| format!("Error parsing unix-group identity: {error}")),
        "unix-netgroup" => {
            let name = details
                .get("name")
                .and_then(|value| <&str>::try_from(value).ok())
                .ok_or("Error parsing net identity: no name")?;
            Ok(Offered::Netgroup(name.to_owned()))
        }
        other => Err(format!("Unknown identity of kind `{other}'")),
    }
}

// Groups and netgroups are skipped, since the helper authenticates only unix users.
pub(crate) fn select(offered: &[Offered], user: &str) -> Option<Identity> {
    let users: Vec<Identity> = offered
        .iter()
        .filter_map(|identity| match identity {
            Offered::User(uid) => Some(Identity { uid: *uid, name: user_name(*uid) }),
            Offered::Group(_) | Offered::Netgroup(_) => None,
        })
        .collect();
    if let Some(own) = users
        .iter()
        .find(|identity| identity.name.as_deref() == Some(user))
    {
        return Some(own.clone());
    }
    let first = users.into_iter().next()?;
    warn!(
        user,
        uid = first.uid,
        "polkitd did not offer the configured user, using the first unix user"
    );
    Some(first)
}

pub(crate) fn user_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0u8; 4096];
    loop {
        // SAFETY: passwd is plain data that getpwuid_r fills in. Its strings point into
        // `buffer`, which outlives every read of them below.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded, so pw_name is a NUL-terminated string in `buffer`.
        let name = unsafe { CStr::from_ptr(entry.pw_name) };
        return name
            .to_str()
            .ok()
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::Variant;

    fn raw(kind: &str, key: &str, value: Variant<'static>) -> RawIdentity {
        (
            kind.to_owned(),
            HashMap::from([(key.to_owned(), value.try_into_owned().unwrap())]),
        )
    }

    #[test]
    fn parses_users_groups_and_netgroups() {
        let parsed = parse(&[
            raw("unix-user", "uid", Variant::from(1000u32)),
            raw("unix-group", "gid", Variant::from(10u32)),
            raw("unix-netgroup", "name", Variant::from("admins")),
        ])
        .unwrap();
        assert_eq!(
            parsed,
            [Offered::User(1000), Offered::Group(10), Offered::Netgroup("admins".into())]
        );
    }

    #[test]
    fn bad_identities_are_errors() {
        let error = parse(&[
            raw("unix-user", "uid", Variant::from(0u32)),
            raw("unix-user", "uid", Variant::from("zero")),
        ])
        .unwrap_err();
        assert!(
            error.starts_with("Error extracting identity 1: "),
            "{error}"
        );
        let error = parse(&[raw("unix-wizard", "x", Variant::from(1u32))]).unwrap_err();
        assert!(
            error.contains("Unknown identity of kind `unix-wizard'"),
            "{error}"
        );
    }

    #[test]
    fn root_has_a_name() {
        assert_eq!(user_name(0).as_deref(), Some("root"));
    }

    #[test]
    fn selection_prefers_the_configured_user() {
        let offered = [Offered::Group(0), Offered::User(u32::MAX - 1), Offered::User(0)];
        assert_eq!(
            select(&offered, "root"),
            Some(Identity { uid: 0, name: Some("root".into()) })
        );
        assert_eq!(
            select(&offered, "nobody-here"),
            Some(Identity { uid: u32::MAX - 1, name: None })
        );
        assert_eq!(select(&[Offered::Group(0)], "root"), None);
    }
}
