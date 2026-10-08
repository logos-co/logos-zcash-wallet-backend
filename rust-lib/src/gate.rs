//! Who may do what, as a pure function of the caller.
//!
//! A role is a set of module names; an empty set admits nobody; the host anchor is
//! always refused; a method the registry does not name is refused outright.

use serde::Deserialize;

/// The wallet app holds both roles by default. Approval asks for the password on
/// every signing, so one surface is enough; the roles stay separate sets so an
/// operator can grant one without the other.
pub const DEFAULT_CUSTODIAN: &str = "zcash_wallet_ui";
pub const DEFAULT_APPROVER: &str = "zcash_wallet_ui";

/// Every refusal, byte for byte as documented; `json!` would sort the keys.
pub const NOT_AUTHORIZED: &str = r#"{"ok":false,"error":"not authorized"}"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    Unknown,
    HostAnchor,
    Module(String),
    Derived { parent: String, leaf: String },
    Operator(String),
}

impl Caller {
    pub fn is_module(&self, name: &str) -> bool {
        matches!(self, Caller::Module(n) if n == name)
    }

    /// Only a plainly named module has a name a request can be recorded against.
    pub fn named(&self) -> Option<&str> {
        match self {
            Caller::Module(n) if !n.is_empty() => Some(n.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roles {
    pub approvers: Vec<String>,
    pub custodians: Vec<String>,
}

impl Default for Roles {
    fn default() -> Self {
        Self { approvers: vec![DEFAULT_APPROVER.into()], custodians: vec![DEFAULT_CUSTODIAN.into()] }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RoleWire {
    One(String),
    Many(Vec<String>),
}

impl Default for RoleWire {
    fn default() -> Self {
        RoleWire::Many(Vec::new())
    }
}

impl RoleWire {
    fn into_holders(self) -> Vec<String> {
        let raw = match self {
            RoleWire::One(s) => vec![s],
            RoleWire::Many(v) => v,
        };
        let mut out: Vec<String> = Vec::with_capacity(raw.len());
        for n in raw {
            let n = n.trim().to_string();
            if !n.is_empty() && !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesWire {
    #[serde(default)]
    approvers: RoleWire,
    #[serde(default)]
    custodians: RoleWire,
}

impl Roles {
    /// Replaces both roles. Total, not a patch: a role the document does not name is
    /// held by nobody. A malformed document is refused and the roles in force stay.
    pub fn configure(&mut self, doc: &str) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_str(doc).map_err(|e| format!("bad roles document: {e}"))?;
        if !v.is_object() {
            return Err("roles document must be an object".into());
        }
        let wire: RolesWire = serde_json::from_value(v).map_err(|e| format!("bad roles document: {e}"))?;
        self.approvers = wire.approvers.into_holders();
        self.custodians = wire.custodians.into_holders();
        Ok(())
    }

    /// Roles at load: absent file means defaults; an unreadable one means nobody.
    pub fn from_file(contents: Option<&str>) -> Self {
        match contents {
            None => Self::default(),
            Some(text) => {
                let mut r = Self { approvers: vec![], custodians: vec![] };
                if r.configure(text).is_err() {
                    return Self { approvers: vec![], custodians: vec![] };
                }
                r
            }
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"approvers": self.approvers, "custodians": self.custodians})
    }
}

fn holds_any(holders: &[String], caller: &Caller) -> bool {
    holders.iter().any(|h| !h.is_empty() && caller.is_module(h))
}

/// Custodian only: anything that takes or reveals a password or a key, and the
/// device-wide network, server and proxy settings. Assigning roles is here too.
pub const CUSTODIAN_METHODS: &[&str] = &[
    "configure",
    "open_wallet",
    "create_wallet",
    "restore_wallet",
    "change_password",
    "reveal_seed",
    "export_viewing_key",
    "set_active_network",
    "set_servers",
    "apply_preset",
    "set_proxy",
    "clear_suspect",
];

/// Approver only: signing with the password.
pub const APPROVER_METHODS: &[&str] = &["approve_send", "approve_migration"];

/// Either role: session and wallet housekeeping that moves no money by itself.
pub const SESSION_METHODS: &[&str] = &[
    "close_wallet",
    "new_address",
    "prepare_shielding",
    "prepare_migration",
    "pause_migration",
    "resume_migration",
    "cancel_migration",
];

pub fn custodian_admits(method: &str, roles: &Roles, caller: &Caller) -> bool {
    CUSTODIAN_METHODS.contains(&method) && holds_any(&roles.custodians, caller)
}

pub fn approver_admits(method: &str, roles: &Roles, caller: &Caller) -> bool {
    APPROVER_METHODS.contains(&method) && holds_any(&roles.approvers, caller)
}

pub fn session_admits(method: &str, roles: &Roles, caller: &Caller) -> bool {
    SESSION_METHODS.contains(&method) && (holds_any(&roles.custodians, caller) || holds_any(&roles.approvers, caller))
}

/// Asking for a send to be built for review: any named module, never the host anchor.
pub fn requester_admits(caller: &Caller) -> bool {
    caller.named().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(n: &str) -> Caller {
        Caller::Module(n.into())
    }

    #[test]
    fn defaults_admit_the_app_only() {
        let r = Roles::default();
        assert!(custodian_admits("open_wallet", &r, &m("zcash_wallet_ui")));
        assert!(approver_admits("approve_send", &r, &m("zcash_wallet_ui")));
        assert!(!custodian_admits("open_wallet", &r, &m("zcash_wallet_cli")));
        assert!(!approver_admits("approve_send", &r, &m("zcash_wallet_cli")));
    }

    #[test]
    fn configure_needs_the_custodian_and_is_total() {
        let mut r = Roles::default();
        assert!(custodian_admits("configure", &r, &m("zcash_wallet_ui")));
        assert!(!custodian_admits("configure", &r, &m("zcash_wallet_cli")));
        r.configure(r#"{"custodians": "zcash_wallet_cli"}"#).unwrap();
        assert!(r.approvers.is_empty());
        assert!(custodian_admits("open_wallet", &r, &m("zcash_wallet_cli")));
        assert!(r.configure(r#"{"custodian": "x"}"#).is_err());
        assert!(r.configure("[]").is_err());
        assert_eq!(r.custodians, vec!["zcash_wallet_cli".to_string()]);
    }

    #[test]
    fn host_anchor_and_strangers_are_refused() {
        let r = Roles::default();
        for method in CUSTODIAN_METHODS {
            assert!(!custodian_admits(method, &r, &Caller::HostAnchor));
        }
        assert!(!session_admits("close_wallet", &r, &Caller::HostAnchor));
        assert!(!requester_admits(&Caller::HostAnchor));
        assert!(!requester_admits(&Caller::Unknown));
        assert!(requester_admits(&m("some_dapp")));
        assert!(!custodian_admits("not_a_method", &r, &m("zcash_wallet_ui")));
    }

    #[test]
    fn refusal_is_exact() {
        assert!(NOT_AUTHORIZED.starts_with(r#"{"ok":false,"#));
        let v: serde_json::Value = serde_json::from_str(NOT_AUTHORIZED).unwrap();
        assert_eq!(v, serde_json::json!({"ok": false, "error": "not authorized"}));
    }

    #[test]
    fn roles_file() {
        assert_eq!(Roles::from_file(None), Roles::default());
        let r = Roles::from_file(Some(r#"{"approvers":["a"],"custodians":["b"]}"#));
        assert_eq!(r.approvers, vec!["a".to_string()]);
        let broken = Roles::from_file(Some("{"));
        assert!(broken.approvers.is_empty() && broken.custodians.is_empty());
    }
}
