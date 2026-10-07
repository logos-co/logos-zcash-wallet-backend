//! Logos glue for `zcash_wallet_backend` (rust-first, `concurrency: "multi"`).
//!
//! The coordinator between the wallet surfaces and the engine. It holds no keys and
//! caches no password: those live only in `zcash_wallet_core_module`. A reactor
//! thread follows core jobs and relays the core's events.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use logos_rust_sdk::{AboutToUnload, LogosCaller, Shutdown};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::gate::{self, Caller, Roles};
use crate::model::{is_network, unwrap_core, Jobs, NETWORKS};

pub trait ZcashWalletBackendModule: Send + Sync + 'static {
    /// CUSTODIAN. Replaces both roles: `{ approvers?, custodians? }`. Total, and saved.
    fn configure(&self, roles_json: String) -> String;
    /// `{ ok, kind, identity, approvers, custodians }`: who this call came from.
    fn caller_identity(&self) -> String;

    /// `{ ok, networks, active }`.
    fn list_networks(&self) -> String;
    /// CUSTODIAN. Refused while a wallet is open.
    fn set_active_network(&self, network: String) -> String;
    /// `{ ok, wallets }` on the active network.
    fn list_wallets(&self) -> String;

    /// CUSTODIAN. Each returns `{ ok, jobId }`; poll job_status.
    fn create_wallet(&self, name: String, password: String) -> String;
    /// `params_json`: `{ name, password, phrase, birthdayHeight }`.
    fn restore_wallet(&self, params_json: String) -> String;
    fn open_wallet(&self, name: String, password: String) -> String;
    fn change_password(&self, old_password: String, new_password: String) -> String;
    /// Either role.
    fn close_wallet(&self) -> String;
    /// `{ ok, jobId, kind, state, result?, error? }`.
    fn job_status(&self, job_id: String) -> String;

    /// CUSTODIAN. Passed to the engine, which checks the password.
    fn reveal_seed(&self, password: String) -> String;
    fn export_viewing_key(&self, password: String) -> String;

    fn wallet_status(&self) -> String;
    fn sync_status(&self) -> String;
    fn balances(&self) -> String;
    /// `{ ok, unified, transparent }`.
    fn receive_info(&self) -> String;
    /// Either role. A new diversified shielded address.
    fn new_address(&self) -> String;

    /// The node module's view, for the active network.
    fn servers(&self) -> String;
    fn server_health(&self) -> String;
    /// CUSTODIAN. Passed to the node module.
    fn apply_preset(&self, name: String) -> String;
    fn set_proxy(&self, config_json: String) -> String;

    fn on_context_ready(&self, _ctx: &RustModuleContext) {}
}

pub trait ZcashWalletBackendModuleEvents {
    fn wallet_state_changed(&self, payload: String);
    fn sync_progress(&self, payload: String);
    fn balance_changed(&self, payload: String);
    fn server_health_changed(&self, payload: String);
    fn job_finished(&self, job_id: String, state: String);
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated/provider_gen.rs"));

#[derive(Default)]
struct Inner {
    jobs: Jobs,
    active: String,
    dir: Option<PathBuf>,
}

pub struct ZcashWalletBackendModuleImpl {
    inner: Arc<Mutex<Inner>>,
    roles: Mutex<Roles>,
    stop: Arc<AtomicBool>,
    reactor: Mutex<Option<JoinHandle<()>>>,
}

impl Default for ZcashWalletBackendModuleImpl {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { active: "mainnet".into(), ..Default::default() })),
            roles: Mutex::new(Roles::default()),
            stop: Arc::new(AtomicBool::new(false)),
            reactor: Mutex::new(None),
        }
    }
}

fn refused() -> String {
    json!({"ok": false, "error": "not authorized"}).to_string()
}

fn err(e: impl std::fmt::Display) -> String {
    json!({"ok": false, "error": e.to_string()}).to_string()
}

fn caller() -> Caller {
    match logos_rust_sdk::current_caller() {
        LogosCaller::Unknown => Caller::Unknown,
        LogosCaller::HostAnchor => Caller::HostAnchor,
        LogosCaller::Module { name, .. } => Caller::Module(name),
        LogosCaller::Derived { parent, leaf } => Caller::Derived { parent, leaf },
        LogosCaller::Operator { name } => Caller::Operator(name),
    }
}

fn parse(raw: Result<String, logos_rust_sdk::LogosError>, who: &str) -> Result<Value, String> {
    let s = raw.map_err(|e| format!("{who}: {e:?}"))?;
    serde_json::from_str(&s).map_err(|e| format!("{who}: {e}"))
}

fn write_atomic(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

impl ZcashWalletBackendModuleImpl {
    fn custodian(&self, method: &str) -> bool {
        gate::custodian_admits(method, &self.roles.lock().unwrap(), &caller())
    }

    fn session(&self, method: &str) -> bool {
        gate::session_admits(method, &self.roles.lock().unwrap(), &caller())
    }

    fn active(&self) -> String {
        self.inner.lock().unwrap().active.clone()
    }

    /// The route table for a network, in the shape the core's jobs take.
    fn routes(&self, network: &str) -> Result<Value, String> {
        let t = parse(modules().zcash_node_module.route_table(network), "node module")?;
        let t = unwrap_core(&t)?;
        let proxy = t.get("proxy").and_then(Value::as_str).filter(|p| !p.is_empty()).ok_or("no proxy is set")?;
        let servers: Vec<String> = t
            .get("sync")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.get("url").and_then(Value::as_str).map(String::from)).collect())
            .unwrap_or_default();
        if servers.is_empty() {
            return Err("no server is enabled".into());
        }
        Ok(json!({"proxy": proxy, "servers": servers}))
    }

    /// Starts a core job and tracks it under a backend id. `params` may hold a
    /// password; it is dropped (zeroized) as soon as the core has it.
    fn start_core_job(&self, kind: &str, params: Zeroizing<String>) -> String {
        let reply = parse(modules().zcash_wallet_core_module.start_job(kind, &params), "wallet core");
        drop(params);
        let v = match reply.and_then(|v| unwrap_core(&v)) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let (Some(id), Some(receipt)) = (v.get("jobId").and_then(Value::as_str), v.get("receipt").and_then(Value::as_str)) else {
            return err("the wallet core returned no job id");
        };
        let jid = self.inner.lock().unwrap().jobs.track(kind, id, receipt);
        json!({"ok": true, "jobId": jid}).to_string()
    }

    fn wallet_job(&self, kind: &str, name: &str, password: Option<Zeroizing<String>>, extra: Value) -> String {
        let network = self.active();
        let routes = match self.routes(&network) {
            Ok(r) => r,
            Err(e) => return err(e),
        };
        let mut p = json!({"network": network, "name": name, "routes": routes});
        if let Some(pw) = password {
            p["password"] = json!(pw.as_str());
        }
        if let (Some(obj), Some(more)) = (p.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                obj.insert(k.clone(), v.clone());
            }
        }
        let params = Zeroizing::new(p.to_string());
        if let Some(v) = p.get_mut("password") {
            *v = Value::Null;
        }
        if let Some(v) = p.get_mut("phrase") {
            *v = Value::Null;
        }
        self.start_core_job(kind, params)
    }

    fn open_name(&self) -> Option<String> {
        let st = parse(modules().zcash_wallet_core_module.wallet_status(), "wallet core").ok()?;
        st.get("name").and_then(Value::as_str).map(String::from)
    }

    fn core_read(&self, r: Result<String, logos_rust_sdk::LogosError>) -> String {
        match r {
            Ok(s) => s,
            Err(e) => err(format!("wallet core: {e:?}")),
        }
    }
}

/// Follows core jobs: on done or failed, reads the result once and acknowledges it.
fn reactor(inner: Arc<Mutex<Inner>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        let pending = inner.lock().unwrap().jobs.pending();
        for (jid, job) in pending {
            let core = modules().zcash_wallet_core_module;
            let status = match parse(core.job_status(&job.core_id, &job.receipt), "wallet core") {
                Ok(s) => s,
                Err(_) => {
                    let mut g = inner.lock().unwrap();
                    if let Some(j) = g.jobs.map.get_mut(&jid) {
                        j.misses += 1;
                        if j.misses >= 10 {
                            j.state = "failed".into();
                            j.error = "the wallet core stopped answering".into();
                            emit_job_finished(&jid, "failed");
                        }
                    }
                    continue;
                }
            };
            let state = status.get("state").and_then(Value::as_str).unwrap_or("failed").to_string();
            if state == "queued" || state == "running" {
                if let Some(j) = inner.lock().unwrap().jobs.map.get_mut(&jid) {
                    j.state = state;
                    j.misses = 0;
                }
                continue;
            }
            let result = parse(core.job_result(&job.core_id, &job.receipt), "wallet core");
            let _ = core.ack_job(&job.core_id, &job.receipt);
            let mut g = inner.lock().unwrap();
            if let Some(j) = g.jobs.map.get_mut(&jid) {
                match result.and_then(|r| unwrap_core(&r)) {
                    Ok(v) if state == "done" => {
                        j.state = "done".into();
                        j.result = v;
                    }
                    Ok(_) => j.state = state.clone(),
                    Err(e) => {
                        j.state = if state == "cancelled" { "cancelled".into() } else { "failed".into() };
                        j.error = e;
                    }
                }
                let final_state = j.state.clone();
                drop(g);
                emit_job_finished(&jid, &final_state);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Relays the core's and the node module's events under the backend's names.
fn relay_events(stop: Arc<AtomicBool>) {
    let core = modules().zcash_wallet_core_module;
    if let Ok(sub) = core.on_sync_progress() {
        let stop = stop.clone();
        std::thread::spawn(move || {
            for ev in sub {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(payload) = ZcashWalletCoreModuleClient::decode_sync_progress(&ev) {
                    emit_sync_progress(&payload);
                }
            }
        });
    }
    if let Ok(sub) = core.on_balance_changed() {
        let stop = stop.clone();
        std::thread::spawn(move || {
            for ev in sub {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(payload) = ZcashWalletCoreModuleClient::decode_balance_changed(&ev) {
                    emit_balance_changed(&payload);
                }
            }
        });
    }
    if let Ok(sub) = core.on_wallet_state_changed() {
        std::thread::spawn(move || {
            for ev in sub {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(payload) = ZcashWalletCoreModuleClient::decode_wallet_state_changed(&ev) {
                    emit_wallet_state_changed(&payload);
                }
            }
        });
    }
}

impl ZcashWalletBackendModule for ZcashWalletBackendModuleImpl {
    fn configure(&self, roles_json: String) -> String {
        if !self.custodian("configure") {
            return refused();
        }
        let mut roles = self.roles.lock().unwrap();
        let mut next = roles.clone();
        if let Err(e) = next.configure(&roles_json) {
            return err(e);
        }
        if let Some(dir) = self.inner.lock().unwrap().dir.clone() {
            if let Err(e) = write_atomic(&dir.join("roles.json"), next.to_json().to_string().as_bytes()) {
                return err(format!("roles not saved: {e}"));
            }
        }
        *roles = next;
        let mut v = roles.to_json();
        v["ok"] = json!(true);
        v.to_string()
    }

    fn caller_identity(&self) -> String {
        let c = logos_rust_sdk::current_caller();
        let roles = self.roles.lock().unwrap();
        json!({"ok": true, "identity": c.identity(), "approvers": roles.approvers, "custodians": roles.custodians}).to_string()
    }

    fn list_networks(&self) -> String {
        json!({"ok": true, "networks": NETWORKS, "active": self.active()}).to_string()
    }

    fn set_active_network(&self, network: String) -> String {
        if !self.custodian("set_active_network") {
            return refused();
        }
        if !is_network(&network) {
            return err("unknown network");
        }
        if self.open_name().is_some() {
            return err("close the wallet first");
        }
        let mut g = self.inner.lock().unwrap();
        g.active = network.clone();
        if let Some(dir) = g.dir.clone() {
            let _ = write_atomic(&dir.join("settings.json"), json!({"activeNetwork": network}).to_string().as_bytes());
        }
        json!({"ok": true, "active": network}).to_string()
    }

    fn list_wallets(&self) -> String {
        self.core_read(modules().zcash_wallet_core_module.list_wallets(&self.active()))
    }

    fn create_wallet(&self, name: String, password: String) -> String {
        let password = Zeroizing::new(password);
        if !self.custodian("create_wallet") {
            return refused();
        }
        self.wallet_job("create_wallet", &name, Some(password), json!({}))
    }

    fn restore_wallet(&self, params_json: String) -> String {
        let raw = Zeroizing::new(params_json);
        if !self.custodian("restore_wallet") {
            return refused();
        }
        let mut v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => return err(format!("params: {e}")),
        };
        let name = v.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let password = Zeroizing::new(v.get("password").and_then(Value::as_str).unwrap_or_default().to_string());
        let phrase = Zeroizing::new(v.get("phrase").and_then(Value::as_str).unwrap_or_default().to_string());
        let birthday = v.get("birthdayHeight").cloned().unwrap_or(Value::Null);
        for k in ["password", "phrase"] {
            if let Some(x) = v.get_mut(k) {
                *x = Value::Null;
            }
        }
        self.wallet_job("restore_wallet", &name, Some(password), json!({"phrase": phrase.as_str(), "birthdayHeight": birthday}))
    }

    fn open_wallet(&self, name: String, password: String) -> String {
        let password = Zeroizing::new(password);
        if !self.custodian("open_wallet") {
            return refused();
        }
        self.wallet_job("open_wallet", &name, Some(password), json!({}))
    }

    fn change_password(&self, old_password: String, new_password: String) -> String {
        let (old, new) = (Zeroizing::new(old_password), Zeroizing::new(new_password));
        if !self.custodian("change_password") {
            return refused();
        }
        let Some(name) = self.open_name() else { return err("no wallet is open") };
        let params = Zeroizing::new(
            json!({"network": self.active(), "name": name, "oldPassword": old.as_str(), "newPassword": new.as_str()}).to_string(),
        );
        self.start_core_job("change_password", params)
    }

    fn close_wallet(&self) -> String {
        if !self.session("close_wallet") {
            return refused();
        }
        self.start_core_job("close_wallet", Zeroizing::new("{}".into()))
    }

    fn job_status(&self, job_id: String) -> String {
        self.inner.lock().unwrap().jobs.status(&job_id).to_string()
    }

    fn reveal_seed(&self, password: String) -> String {
        let password = Zeroizing::new(password);
        if !self.custodian("reveal_seed") {
            return refused();
        }
        self.core_read(modules().zcash_wallet_core_module.reveal_seed(&password))
    }

    fn export_viewing_key(&self, password: String) -> String {
        let password = Zeroizing::new(password);
        if !self.custodian("export_viewing_key") {
            return refused();
        }
        self.core_read(modules().zcash_wallet_core_module.export_viewing_key("", &password))
    }

    fn wallet_status(&self) -> String {
        self.core_read(modules().zcash_wallet_core_module.wallet_status())
    }

    fn sync_status(&self) -> String {
        self.core_read(modules().zcash_wallet_core_module.sync_status())
    }

    fn balances(&self) -> String {
        self.core_read(modules().zcash_wallet_core_module.balances(""))
    }

    fn receive_info(&self) -> String {
        self.core_read(modules().zcash_wallet_core_module.addresses(""))
    }

    fn new_address(&self) -> String {
        if !self.session("new_address") {
            return refused();
        }
        self.core_read(modules().zcash_wallet_core_module.new_address(""))
    }

    fn servers(&self) -> String {
        self.core_read(modules().zcash_node_module.servers(&self.active()))
    }

    fn server_health(&self) -> String {
        self.core_read(modules().zcash_node_module.server_health(&self.active()))
    }

    fn apply_preset(&self, name: String) -> String {
        if !self.custodian("apply_preset") {
            return refused();
        }
        self.core_read(modules().zcash_node_module.apply_preset(&self.active(), &name))
    }

    fn set_proxy(&self, config_json: String) -> String {
        if !self.custodian("set_proxy") {
            return refused();
        }
        self.core_read(modules().zcash_node_module.set_proxy(&self.active(), &config_json))
    }

    fn on_context_ready(&self, ctx: &RustModuleContext) {
        let dir = PathBuf::from(&ctx.instance_persistence_path);
        let roles = std::fs::read_to_string(dir.join("roles.json")).ok();
        *self.roles.lock().unwrap() = Roles::from_file(roles.as_deref());
        {
            let mut g = self.inner.lock().unwrap();
            if let Ok(s) = std::fs::read_to_string(dir.join("settings.json")) {
                if let Some(n) = serde_json::from_str::<Value>(&s).ok().and_then(|v| v["activeNetwork"].as_str().map(String::from)) {
                    if is_network(&n) {
                        g.active = n;
                    }
                }
            }
            g.dir = Some(dir);
        }
        let (inner, stop) = (self.inner.clone(), self.stop.clone());
        // Calls out wait until after the host has finished wiring this module.
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            relay_events(stop.clone());
            reactor(inner, stop);
        });
        *self.reactor.lock().unwrap() = Some(handle);
    }
}

impl AboutToUnload for ZcashWalletBackendModuleImpl {
    fn about_to_unload(&self) -> Shutdown {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.reactor.lock().unwrap().take() {
            let _ = h.join();
        }
        Shutdown::Synchronous
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    logos_install!(ZcashWalletBackendModuleImpl);
}
