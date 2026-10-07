//! Backend state that needs no Logos: tracked core jobs, the active network, and
//! amount formatting.

use std::collections::HashMap;

use serde_json::{json, Value};

pub const NETWORKS: &[&str] = &["mainnet", "testnet"];

pub fn is_network(n: &str) -> bool {
    NETWORKS.contains(&n)
}

/// A core job this backend started. The UI only ever sees the backend's own id,
/// so a job id that crosses the event plane authorises nothing.
#[derive(Debug, Clone)]
pub struct TrackedJob {
    pub kind: String,
    pub core_id: String,
    pub receipt: String,
    pub state: String,
    pub result: Value,
    pub error: String,
    pub misses: u32,
}

#[derive(Default)]
pub struct Jobs {
    next: u64,
    pub map: HashMap<String, TrackedJob>,
}

impl Jobs {
    pub fn track(&mut self, kind: &str, core_id: &str, receipt: &str) -> String {
        self.next += 1;
        let id = format!("b{}", self.next);
        self.map.insert(
            id.clone(),
            TrackedJob {
                kind: kind.into(),
                core_id: core_id.into(),
                receipt: receipt.into(),
                state: "queued".into(),
                result: Value::Null,
                error: String::new(),
                misses: 0,
            },
        );
        id
    }

    pub fn status(&self, id: &str) -> Value {
        match self.map.get(id) {
            None => json!({"ok": false, "error": "unknown job"}),
            Some(j) => {
                let mut v = json!({"ok": true, "jobId": id, "kind": j.kind, "state": j.state});
                if j.state == "done" {
                    v["result"] = j.result.clone();
                }
                if j.state == "failed" || j.state == "cancelled" {
                    v["error"] = json!(j.error);
                }
                v
            }
        }
    }

    pub fn pending(&self) -> Vec<(String, TrackedJob)> {
        self.map
            .iter()
            .filter(|(_, j)| j.state == "queued" || j.state == "running")
            .map(|(k, j)| (k.clone(), j.clone()))
            .collect()
    }
}

const ZAT_PER_ZEC: u64 = 100_000_000;

/// Zatoshis as a ZEC decimal string with trailing zeros trimmed: 123450000 → "1.2345".
pub fn format_zec(zat: u64) -> String {
    let whole = zat / ZAT_PER_ZEC;
    let frac = zat % ZAT_PER_ZEC;
    if frac == 0 {
        return whole.to_string();
    }
    let f = format!("{frac:08}");
    format!("{whole}.{}", f.trim_end_matches('0'))
}

/// A ZEC decimal string as zatoshis. At most 8 decimals; no signs, no exponents.
pub fn parse_zec(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (w, f) = s.split_once('.').unwrap_or((s, ""));
    if w.is_empty() && f.is_empty() || !w.chars().all(|c| c.is_ascii_digit()) || !f.chars().all(|c| c.is_ascii_digit()) {
        return Err("not a ZEC amount".into());
    }
    if f.len() > 8 {
        return Err("at most 8 decimal places".into());
    }
    let whole: u64 = if w.is_empty() { 0 } else { w.parse().map_err(|_| "amount too large".to_string())? };
    let frac: u64 = format!("{f:0<8}").parse().map_err(|_| "not a ZEC amount".to_string())?;
    whole
        .checked_mul(ZAT_PER_ZEC)
        .and_then(|z| z.checked_add(frac))
        .filter(|z| *z <= 21_000_000 * ZAT_PER_ZEC)
        .ok_or_else(|| "more than the ZEC supply".into())
}

/// A core reply is `{ ok, result }` or `{ ok:false, error }`; take the result.
pub fn unwrap_core(v: &Value) -> Result<Value, String> {
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v.get("result").cloned().unwrap_or_else(|| v.clone())),
        Some(false) => Err(v.get("error").and_then(Value::as_str).unwrap_or("wallet core error").to_string()),
        None => Err("unexpected reply from the wallet core".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zec_amounts() {
        assert_eq!(format_zec(123_450_000), "1.2345");
        assert_eq!(format_zec(100_000_000), "1");
        assert_eq!(format_zec(1), "0.00000001");
        assert_eq!(parse_zec("1.2345").unwrap(), 123_450_000);
        assert_eq!(parse_zec(".5").unwrap(), 50_000_000);
        assert_eq!(parse_zec("0.00000001").unwrap(), 1);
        assert!(parse_zec("0.000000001").is_err());
        assert!(parse_zec("-1").is_err());
        assert!(parse_zec("1e3").is_err());
        assert!(parse_zec("21000001").is_err());
        assert!(parse_zec("").is_err());
    }

    #[test]
    fn job_status_hides_core_receipts() {
        let mut j = Jobs::default();
        let id = j.track("open_wallet", "j1", "secret-receipt");
        let s = j.status(&id).to_string();
        assert!(!s.contains("secret-receipt") && !s.contains("\"j1\""));
        assert_eq!(j.pending().len(), 1);
    }
}

/// One send, from request to settlement. Only one may be open per wallet.
#[derive(Debug, Clone)]
pub struct Send {
    pub requester: String,
    pub state: String,
    pub created: std::time::Instant,
    pub previewed_at: Option<std::time::Instant>,
    pub proposal_id: String,
    pub preview: Value,
    pub job: Option<String>,
    pub result: Value,
    pub error: String,
}

pub const PREVIEW_TTL_SECS: u64 = 120;

#[derive(Default)]
pub struct Sends {
    next: u64,
    pub map: HashMap<String, Send>,
}

impl Sends {
    /// The open send, if any: anything not yet settled.
    pub fn open(&self) -> Option<&str> {
        self.map
            .iter()
            .find(|(_, s)| matches!(s.state.as_str(), "preparing" | "previewed" | "signing"))
            .map(|(k, _)| k.as_str())
    }

    pub fn add(&mut self, requester: &str, job: &str) -> String {
        self.next += 1;
        let id = format!("s{}", self.next);
        self.map.insert(
            id.clone(),
            Send {
                requester: requester.into(),
                state: "preparing".into(),
                created: std::time::Instant::now(),
                previewed_at: None,
                proposal_id: String::new(),
                preview: Value::Null,
                job: Some(job.into()),
                result: Value::Null,
                error: String::new(),
            },
        );
        id
    }

    /// Previews older than the TTL fail; returns the ids that changed.
    pub fn expire(&mut self) -> Vec<String> {
        let mut changed = vec![];
        for (id, s) in self.map.iter_mut() {
            if s.state == "previewed" && s.previewed_at.is_some_and(|t| t.elapsed().as_secs() > PREVIEW_TTL_SECS) {
                s.state = "expired".into();
                s.error = "the preview expired before approval".into();
                changed.push(id.clone());
            }
        }
        changed
    }

    pub fn status(&self, id: &str) -> Value {
        match self.map.get(id) {
            None => json!({"ok": false, "error": "unknown send"}),
            Some(s) => json!({
                "ok": true, "requestId": id, "state": s.state, "requester": s.requester,
                "preview": s.preview, "result": s.result, "error": s.error,
                "ttlSecs": s.previewed_at.map(|t| PREVIEW_TTL_SECS.saturating_sub(t.elapsed().as_secs())),
            }),
        }
    }
}

#[cfg(test)]
mod send_tests {
    use super::*;

    #[test]
    fn one_open_send() {
        let mut s = Sends::default();
        assert!(s.open().is_none());
        let id = s.add("some_dapp", "b1");
        assert_eq!(s.open(), Some(id.as_str()));
        s.map.get_mut(&id).unwrap().state = "sent".into();
        assert!(s.open().is_none());
    }

    #[test]
    fn previews_expire() {
        let mut s = Sends::default();
        let id = s.add("x", "b1");
        let e = s.map.get_mut(&id).unwrap();
        e.state = "previewed".into();
        e.previewed_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(PREVIEW_TTL_SECS + 1));
        assert_eq!(s.expire(), vec![id.clone()]);
        assert_eq!(s.status(&id)["state"], "expired");
    }
}
