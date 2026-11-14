//! DNS zone editor for domains hosted at Cloudflare or Hostinger, an email
//! DNS wizard (MX / SPF / DKIM / DMARC for an outside mail provider) and
//! wildcard certificates through Cloudflare DNS.
//!
//! API tokens are stored in the settings table and never sent back to the
//! browser. Calls go through curl with the token in a config file on stdin
//! (never in argv). Base URLs can be overridden for tests:
//! `NEXACLOUD_CLOUDFLARE_API`, `NEXACLOUD_HOSTINGER_API`.

use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{audited, AuditInfo};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::util::cmd::Cmd;

const KEY: &str = "dns_providers";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Provider {
    pub id: String,
    /// cloudflare | hostinger
    pub kind: String,
    pub name: String,
    pub token: String,
    pub created_at: String,
}

fn load(state: &AppState) -> Vec<Provider> {
    state.db.setting(KEY).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(state: &AppState, v: &[Provider]) -> ApiResult<()> {
    state.db.set_setting(KEY, &serde_json::to_string(v).unwrap_or_else(|_| "[]".into())).map_err(|e| ApiError::internal(format!("{e:#}")))
}

fn public(p: &Provider) -> Value {
    json!({"id": p.id, "kind": p.kind, "name": p.name, "token_hint": format!("…{}", &p.token[p.token.len().saturating_sub(4)..]), "created_at": p.created_at})
}

fn cf_api() -> String {
    std::env::var("NEXACLOUD_CLOUDFLARE_API").unwrap_or_else(|_| "https://api.cloudflare.com/client/v4".into())
}
fn hs_api() -> String {
    std::env::var("NEXACLOUD_HOSTINGER_API").unwrap_or_else(|_| "https://developers.hostinger.com".into())
}

/// HTTP JSON call with a bearer token (token never in argv). Returns (status, body).
async fn call(method: &str, url: &str, token: &str, body: Option<&Value>) -> ApiResult<(u16, Value)> {
    let mut cfg = format!(
        "url = {}\nrequest = {}\nheader = {}\nheader = \"Accept: application/json\"\nsilent\nshow-error\nmax-time = 30\nwrite-out = \"\\n%{{http_code}}\"\n",
        crate::monitor::notify::cq(url),
        crate::monitor::notify::cq(method),
        crate::monitor::notify::cq(&format!("Authorization: Bearer {token}")),
    );
    if let Some(b) = body {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
        cfg.push_str(&format!("data-binary = {}\n", crate::monitor::notify::cq(&b.to_string())));
    }
    let o = Cmd::new("curl").args(["-K", "-"]).stdin(cfg.into_bytes()).timeout(Duration::from_secs(40)).run().await;
    let out = o.stdout.replace(token, "•••");
    let (text, code) = out.rsplit_once('\n').unwrap_or(("", out.as_str()));
    let status: u16 = code.trim().parse().unwrap_or(0);
    if status == 0 {
        return Err(ApiError::bad_request(format!("could not reach the DNS provider: {}", crate::util::cmd::truncate(o.stderr.trim(), 200))));
    }
    Ok((status, serde_json::from_str(text).unwrap_or(Value::Null)))
}

fn provider_error(status: u16, v: &Value) -> ApiError {
    let msg = v["errors"][0]["message"].as_str().or(v["message"].as_str()).or(v["error"].as_str()).unwrap_or("request refused");
    let hint = if status == 401 || status == 403 { " — check the API token and its permissions" } else { "" };
    ApiError::bad_request(format!("DNS provider said ({status}): {msg}{hint}"))
}

// ------------------------------------------------------------------ providers

pub async fn providers(State(state): State<AppState>) -> Json<Value> {
    Json(json!(load(&state).iter().map(public).collect::<Vec<_>>()))
}

#[derive(Deserialize)]
pub struct NewProvider {
    kind: String,
    name: String,
    token: String,
}

/// Check a token works (Cloudflare: token verify; Hostinger: list domains).
async fn verify(kind: &str, token: &str) -> ApiResult<()> {
    let (s, v) = match kind {
        "cloudflare" => call("GET", &format!("{}/user/tokens/verify", cf_api()), token, None).await?,
        _ => call("GET", &format!("{}/api/domains/v1/portfolio", hs_api()), token, None).await?,
    };
    if (200..300).contains(&s) {
        Ok(())
    } else {
        Err(provider_error(s, &v))
    }
}

pub async fn provider_add(State(state): State<AppState>, Json(b): Json<NewProvider>) -> Response {
    let info = AuditInfo::action("dns.provider_add", format!("{}: {}", b.kind, b.name));
    let res = async {
        if !["cloudflare", "hostinger"].contains(&b.kind.as_str()) {
            return Err(ApiError::bad_request("provider: cloudflare or hostinger"));
        }
        let token = b.token.trim().to_string();
        if token.len() < 20 || token.len() > 300 || !token.bytes().all(|c| c.is_ascii_graphic()) {
            return Err(ApiError::bad_request("paste the API token"));
        }
        let name = b.name.trim();
        if name.is_empty() || name.len() > 60 {
            return Err(ApiError::bad_request("give it a name (1–60 characters)"));
        }
        verify(&b.kind, &token).await?;
        let mut v = load(&state);
        let p = Provider { id: uuid::Uuid::new_v4().simple().to_string()[..10].to_string(), kind: b.kind.clone(), name: name.into(), token, created_at: crate::util::fmt_ts(chrono::Utc::now()) };
        v.push(p.clone());
        save(&state, &v)?;
        Ok(Json(public(&p)).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn provider_remove(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let info = AuditInfo::action("dns.provider_remove", id.clone());
    let res = (|| {
        let mut v = load(&state);
        let n = v.len();
        v.retain(|p| p.id != id);
        if v.len() == n {
            return Err(ApiError::not_found("no such DNS provider"));
        }
        save(&state, &v)?;
        let _ = std::fs::remove_file(cf_ini(&id));
        Ok(Json(json!({"ok": true})).into_response())
    })();
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

fn get_provider(state: &AppState, id: &str) -> ApiResult<Provider> {
    load(state).into_iter().find(|p| p.id == id).ok_or_else(|| ApiError::not_found("no such DNS provider"))
}

// ------------------------------------------------------------------ records

/// One record in a provider-neutral shape. `name` is relative ("@" = apex).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub content: String,
    #[serde(default = "auto_ttl")]
    pub ttl: u32,
    #[serde(default)]
    pub priority: Option<u16>,
    #[serde(default)]
    pub proxied: Option<bool>,
}
fn auto_ttl() -> u32 {
    3600
}

const TYPES: [&str; 8] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SRV", "CAA"];

fn valid_domain_zone(d: &str) -> bool {
    crate::plans::sites::valid_domain(d)
}

pub fn validate(r: &Record) -> Result<(), String> {
    if !TYPES.contains(&r.kind.as_str()) {
        return Err(format!("type: one of {}", TYPES.join(", ")));
    }
    let n = r.name.trim();
    if n.is_empty() || n.len() > 200 || !(n == "@" || n.split('.').all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'*'))) {
        return Err("name: @ for the domain itself, or a label like www, mail, *.dev".into());
    }
    let c = r.content.trim();
    if c.is_empty() || c.len() > 2048 || c.contains(['\n', '\r']) {
        return Err("value: one line, up to 2048 characters".into());
    }
    match r.kind.as_str() {
        "A" if c.parse::<std::net::Ipv4Addr>().is_err() => return Err("A record: an IPv4 address".into()),
        "AAAA" if c.parse::<std::net::Ipv6Addr>().is_err() => return Err("AAAA record: an IPv6 address".into()),
        "CNAME" | "NS" | "MX" if !valid_domain_zone(c.trim_end_matches('.')) => return Err(format!("{} record: a host name like target.example.com", r.kind)),
        _ => {}
    }
    if r.kind == "MX" && r.priority.is_none() {
        return Err("MX record: give a priority (e.g. 10)".into());
    }
    if r.ttl != 1 && !(60..=86400).contains(&r.ttl) {
        return Err("TTL: 60–86400 seconds (or 1 = automatic on Cloudflare)".into());
    }
    Ok(())
}

fn fqdn(name: &str, zone: &str) -> String {
    if name == "@" { zone.to_string() } else { format!("{name}.{zone}") }
}
fn relative(name: &str, zone: &str) -> String {
    let n = name.trim_end_matches('.');
    if n.eq_ignore_ascii_case(zone) {
        "@".into()
    } else {
        n.strip_suffix(&format!(".{zone}")).unwrap_or(n).to_string()
    }
}

async fn cf_zone_id(token: &str, zone: &str) -> ApiResult<String> {
    let (s, v) = call("GET", &format!("{}/zones?name={zone}", cf_api()), token, None).await?;
    if !(200..300).contains(&s) {
        return Err(provider_error(s, &v));
    }
    v["result"][0]["id"].as_str().map(str::to_string).ok_or_else(|| ApiError::not_found(format!("{zone} is not in this Cloudflare account")))
}

async fn list_records(p: &Provider, zone: &str) -> ApiResult<Vec<Record>> {
    if p.kind == "cloudflare" {
        let zid = cf_zone_id(&p.token, zone).await?;
        let (s, v) = call("GET", &format!("{}/zones/{zid}/dns_records?per_page=500", cf_api()), &p.token, None).await?;
        if !(200..300).contains(&s) {
            return Err(provider_error(s, &v));
        }
        Ok(v["result"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| Record {
                        id: r["id"].as_str().unwrap_or("").into(),
                        kind: r["type"].as_str().unwrap_or("").into(),
                        name: relative(r["name"].as_str().unwrap_or(""), zone),
                        content: r["content"].as_str().unwrap_or("").into(),
                        ttl: r["ttl"].as_u64().unwrap_or(1) as u32,
                        priority: r["priority"].as_u64().map(|x| x as u16),
                        proxied: r["proxied"].as_bool(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    } else {
        let (s, v) = call("GET", &format!("{}/api/dns/v1/zones/{zone}", hs_api()), &p.token, None).await?;
        if !(200..300).contains(&s) {
            return Err(provider_error(s, &v));
        }
        let mut out = Vec::new();
        for set in v.as_array().cloned().unwrap_or_default() {
            let kind = set["type"].as_str().unwrap_or("").to_string();
            let name = set["name"].as_str().unwrap_or("@").to_string();
            let ttl = set["ttl"].as_u64().unwrap_or(3600) as u32;
            for (i, r) in set["records"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                let content = r["content"].as_str().unwrap_or("").to_string();
                let (priority, content) = if kind == "MX" {
                    match content.split_once(' ') {
                        Some((p, h)) if p.parse::<u16>().is_ok() => (p.parse().ok(), h.to_string()),
                        _ => (None, content),
                    }
                } else {
                    (None, content)
                };
                out.push(Record { id: format!("{name}|{kind}|{i}"), kind: kind.clone(), name: if name.is_empty() { "@".into() } else { name.clone() }, content, ttl, priority, proxied: None });
            }
        }
        Ok(out)
    }
}

/// Hostinger works on record sets (name + type): write the whole set.
async fn hs_put_set(p: &Provider, zone: &str, name: &str, kind: &str, contents: &[(Option<u16>, String)], ttl: u32) -> ApiResult<()> {
    let records: Vec<Value> = contents.iter().map(|(pr, c)| json!({"content": match pr { Some(x) => format!("{x} {c}"), None => c.clone() }})).collect();
    let (s, v) = if records.is_empty() {
        call("DELETE", &format!("{}/api/dns/v1/zones/{zone}", hs_api()), &p.token, Some(&json!({"filters": [{"name": name, "type": kind}]}))).await?
    } else {
        call("PUT", &format!("{}/api/dns/v1/zones/{zone}", hs_api()), &p.token, Some(&json!({"overwrite": true, "zone": [{"name": name, "type": kind, "ttl": ttl, "records": records}]}))).await?
    };
    if (200..300).contains(&s) { Ok(()) } else { Err(provider_error(s, &v)) }
}

fn cf_body(r: &Record, zone: &str) -> Value {
    let mut b = json!({"type": r.kind, "name": fqdn(&r.name, zone), "content": r.content.trim(), "ttl": r.ttl});
    if let Some(p) = r.priority {
        b["priority"] = json!(p);
    }
    if ["A", "AAAA", "CNAME"].contains(&r.kind.as_str()) {
        b["proxied"] = json!(r.proxied.unwrap_or(false));
    }
    b
}

async fn upsert(p: &Provider, zone: &str, r: &Record, existing_id: Option<&str>) -> ApiResult<()> {
    validate(r).map_err(ApiError::bad_request)?;
    if p.kind == "cloudflare" {
        let zid = cf_zone_id(&p.token, zone).await?;
        let (s, v) = match existing_id {
            Some(id) => call("PUT", &format!("{}/zones/{zid}/dns_records/{id}", cf_api()), &p.token, Some(&cf_body(r, zone))).await?,
            None => call("POST", &format!("{}/zones/{zid}/dns_records", cf_api()), &p.token, Some(&cf_body(r, zone))).await?,
        };
        return if (200..300).contains(&s) { Ok(()) } else { Err(provider_error(s, &v)) };
    }
    let all = list_records(p, zone).await?;
    // the set this record belongs to, minus the record being replaced
    let mut set: Vec<(Option<u16>, String)> = all.iter().filter(|x| x.name == r.name && x.kind == r.kind && Some(x.id.as_str()) != existing_id).map(|x| (x.priority, x.content.clone())).collect();
    set.push((r.priority, r.content.trim().to_string()));
    if let Some(old) = existing_id.and_then(|id| all.iter().find(|x| x.id == id)) {
        if old.name != r.name || old.kind != r.kind {
            let rest: Vec<(Option<u16>, String)> = all.iter().filter(|x| x.name == old.name && x.kind == old.kind && x.id != old.id).map(|x| (x.priority, x.content.clone())).collect();
            hs_put_set(p, zone, &old.name, &old.kind, &rest, old.ttl).await?;
        }
    }
    hs_put_set(p, zone, &r.name, &r.kind, &set, r.ttl).await
}

async fn delete_record(p: &Provider, zone: &str, id: &str) -> ApiResult<()> {
    if p.kind == "cloudflare" {
        let zid = cf_zone_id(&p.token, zone).await?;
        let (s, v) = call("DELETE", &format!("{}/zones/{zid}/dns_records/{id}", cf_api()), &p.token, None).await?;
        return if (200..300).contains(&s) { Ok(()) } else { Err(provider_error(s, &v)) };
    }
    let all = list_records(p, zone).await?;
    let old = all.iter().find(|x| x.id == id).ok_or_else(|| ApiError::not_found("record not found (the zone changed — reload)"))?;
    let rest: Vec<(Option<u16>, String)> = all.iter().filter(|x| x.name == old.name && x.kind == old.kind && x.id != old.id).map(|x| (x.priority, x.content.clone())).collect();
    hs_put_set(p, zone, &old.name, &old.kind, &rest, old.ttl).await
}

fn check_zone(zone: &str) -> ApiResult<()> {
    if valid_domain_zone(zone) { Ok(()) } else { Err(ApiError::bad_request("invalid domain")) }
}

/// `GET /api/dns/{pid}/zones/{zone}/records`
pub async fn records(State(state): State<AppState>, UrlPath((pid, zone)): UrlPath<(String, String)>) -> ApiResult<Json<Value>> {
    check_zone(&zone)?;
    let p = get_provider(&state, &pid)?;
    let mut v = list_records(&p, &zone).await?;
    v.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(json!({"zone": zone, "provider": public(&p), "records": v})))
}

pub async fn record_create(State(state): State<AppState>, UrlPath((pid, zone)): UrlPath<(String, String)>, Json(r): Json<Record>) -> Response {
    let info = AuditInfo::action("dns.record_create", format!("{zone}: {} {}", r.kind, r.name));
    let res: ApiResult<Response> = async {
        check_zone(&zone)?;
        let p = get_provider(&state, &pid)?;
        upsert(&p, &zone, &r, None).await?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn record_update(State(state): State<AppState>, UrlPath((pid, zone, id)): UrlPath<(String, String, String)>, Json(r): Json<Record>) -> Response {
    let info = AuditInfo::action("dns.record_update", format!("{zone}: {} {}", r.kind, r.name));
    let res: ApiResult<Response> = async {
        check_zone(&zone)?;
        let p = get_provider(&state, &pid)?;
        upsert(&p, &zone, &r, Some(&id)).await?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

pub async fn record_delete(State(state): State<AppState>, UrlPath((pid, zone, id)): UrlPath<(String, String, String)>) -> Response {
    let info = AuditInfo::action("dns.record_delete", format!("{zone}: {id}"));
    let res: ApiResult<Response> = async {
        check_zone(&zone)?;
        let p = get_provider(&state, &pid)?;
        delete_record(&p, &zone, &id).await?;
        Ok(Json(json!({"ok": true})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ email DNS wizard

#[derive(Deserialize, Clone)]
pub struct EmailSetup {
    /// google | microsoft | zoho | fastmail | custom
    pub provider: String,
    /// custom: MX hosts as "10 mx1.example.net"
    #[serde(default)]
    pub mx: Vec<String>,
    /// custom: SPF include (e.g. spf.example.net)
    #[serde(default)]
    pub spf_include: Option<String>,
    /// DKIM selector + public key value from the mail provider (optional)
    #[serde(default)]
    pub dkim_selector: Option<String>,
    #[serde(default)]
    pub dkim_value: Option<String>,
    /// none | quarantine | reject
    #[serde(default = "dmarc_none")]
    pub dmarc_policy: String,
    #[serde(default)]
    pub dmarc_email: Option<String>,
}
fn dmarc_none() -> String {
    "none".into()
}

/// Records for a mail provider. Microsoft's MX depends on the domain.
pub fn email_records(zone: &str, e: &EmailSetup) -> Result<Vec<Record>, String> {
    let rec = |kind: &str, name: &str, content: String, priority: Option<u16>| Record { id: String::new(), kind: kind.into(), name: name.into(), content, ttl: 3600, priority, proxied: None };
    let (mx, spf): (Vec<(u16, String)>, String) = match e.provider.as_str() {
        "google" => (vec![(1, "smtp.google.com".into())], "_spf.google.com".into()),
        "microsoft" => (vec![(0, format!("{}.mail.protection.outlook.com", zone.replace('.', "-")))], "spf.protection.outlook.com".into()),
        "zoho" => (vec![(10, "mx.zoho.com".into()), (20, "mx2.zoho.com".into()), (50, "mx3.zoho.com".into())], "zoho.com".into()),
        "fastmail" => (vec![(10, "in1-smtp.messagingengine.com".into()), (20, "in2-smtp.messagingengine.com".into())], "spf.messagingengine.com".into()),
        "custom" => {
            let mut v = Vec::new();
            for m in &e.mx {
                let (p, h) = m.trim().split_once(' ').ok_or("MX lines look like: 10 mx1.example.net")?;
                v.push((p.parse::<u16>().map_err(|_| "MX priority must be a number")?, h.trim().trim_end_matches('.').to_string()));
            }
            if v.is_empty() {
                return Err("add at least one MX host".into());
            }
            (v, e.spf_include.clone().unwrap_or_default())
        }
        _ => return Err("mail provider: google, microsoft, zoho, fastmail or custom".into()),
    };
    let mut out: Vec<Record> = mx.into_iter().map(|(p, h)| rec("MX", "@", h, Some(p))).collect();
    let spf_txt = if spf.trim().is_empty() { "v=spf1 mx ~all".to_string() } else { format!("v=spf1 include:{} ~all", spf.trim()) };
    out.push(rec("TXT", "@", spf_txt, None));
    if let (Some(sel), Some(val)) = (&e.dkim_selector, &e.dkim_value) {
        let sel = sel.trim();
        if !sel.is_empty() && !val.trim().is_empty() {
            if !sel.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err("DKIM selector: letters, digits, - or _".into());
            }
            out.push(rec("TXT", &format!("{sel}._domainkey"), val.trim().to_string(), None));
        }
    }
    if !["none", "quarantine", "reject"].contains(&e.dmarc_policy.as_str()) {
        return Err("DMARC policy: none, quarantine or reject".into());
    }
    let mut dmarc = format!("v=DMARC1; p={}", e.dmarc_policy);
    if let Some(m) = e.dmarc_email.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        if !crate::monitor::notify::valid_email(m) {
            return Err("DMARC report address is not a valid email".into());
        }
        dmarc.push_str(&format!("; rua=mailto:{m}"));
    }
    out.push(rec("TXT", "_dmarc", dmarc, None));
    for r in &out {
        validate(r)?;
    }
    Ok(out)
}

/// `POST /api/dns/email/preview/{zone}` — the records, to copy by hand or apply.
pub async fn email_preview(UrlPath(zone): UrlPath<String>, Json(e): Json<EmailSetup>) -> ApiResult<Json<Value>> {
    check_zone(&zone)?;
    Ok(Json(json!({"records": email_records(&zone, &e).map_err(ApiError::bad_request)?})))
}

/// `POST /api/dns/{pid}/zones/{zone}/email` — replace MX, the SPF TXT, the
/// DMARC TXT (and add DKIM) at the provider.
pub async fn email_apply(State(state): State<AppState>, UrlPath((pid, zone)): UrlPath<(String, String)>, Json(e): Json<EmailSetup>) -> Response {
    let info = AuditInfo::action("dns.email_setup", format!("{zone}: {}", e.provider));
    let res: ApiResult<Response> = async {
        check_zone(&zone)?;
        let p = get_provider(&state, &pid)?;
        let wanted = email_records(&zone, &e).map_err(ApiError::bad_request)?;
        let existing = list_records(&p, &zone).await?;
        // remove what the new setup replaces
        let replaced: Vec<&Record> = existing
            .iter()
            .filter(|r| (r.kind == "MX" && r.name == "@") || (r.kind == "TXT" && r.name == "@" && r.content.trim_matches('"').starts_with("v=spf1")) || (r.kind == "TXT" && r.name == "_dmarc"))
            .collect();
        if p.kind == "cloudflare" {
            for r in &replaced {
                delete_record(&p, &zone, &r.id).await?;
            }
            for r in &wanted {
                upsert(&p, &zone, r, None).await?;
            }
        } else {
            // Hostinger: write whole sets — MX, the @ TXT set (keep non-SPF TXT) and the rest
            let keep_txt: Vec<(Option<u16>, String)> = existing.iter().filter(|r| r.kind == "TXT" && r.name == "@" && !r.content.trim_matches('"').starts_with("v=spf1")).map(|r| (None, r.content.clone())).collect();
            let mx: Vec<(Option<u16>, String)> = wanted.iter().filter(|r| r.kind == "MX").map(|r| (r.priority, r.content.clone())).collect();
            hs_put_set(&p, &zone, "@", "MX", &mx, 3600).await?;
            let mut txt = keep_txt;
            txt.extend(wanted.iter().filter(|r| r.kind == "TXT" && r.name == "@").map(|r| (None, r.content.clone())));
            hs_put_set(&p, &zone, "@", "TXT", &txt, 3600).await?;
            for r in wanted.iter().filter(|r| r.name != "@") {
                hs_put_set(&p, &zone, &r.name, &r.kind, &[(None, r.content.clone())], r.ttl).await?;
            }
        }
        Ok(Json(json!({"ok": true, "records": wanted})).into_response())
    }
    .await;
    audited(res.unwrap_or_else(IntoResponse::into_response), info)
}

// ------------------------------------------------------------------ wildcard SSL (Cloudflare DNS-01)

fn cf_ini(pid: &str) -> String {
    format!("/etc/nexacloud/dns/cloudflare-{pid}.ini")
}

#[derive(Deserialize)]
pub struct WildcardBody {
    zone: String,
    email: String,
    /// sites that should use the new certificate
    #[serde(default)]
    sites: Vec<String>,
}

/// `POST /api/dns/{pid}/wildcard` — job: certbot with the Cloudflare DNS
/// plugin for `zone` and `*.zone`, then point the chosen sites at it.
pub async fn wildcard(State(state): State<AppState>, axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>, UrlPath(pid): UrlPath<String>, Json(b): Json<WildcardBody>) -> Response {
    let mut info = AuditInfo::action("dns.wildcard_ssl", b.zone.clone());
    let res: ApiResult<String> = async {
        check_zone(&b.zone)?;
        let p = get_provider(&state, &pid)?;
        if p.kind != "cloudflare" {
            return Err(ApiError::bad_request("wildcard certificates need a Cloudflare DNS connection (Hostinger has no certbot plugin) — or upload your own wildcard certificate on the site's Settings"));
        }
        if !crate::monitor::notify::valid_email(&b.email) {
            return Err(ApiError::bad_request("enter a valid email address for Let's Encrypt"));
        }
        for s in &b.sites {
            let m = crate::plans::sites::load_model(&state.changes_env, s).map_err(ApiError::bad_request)?;
            if !(m.id == b.zone || m.id.ends_with(&format!(".{}", b.zone))) || m.id.matches('.').count() > b.zone.matches('.').count() + 1 {
                return Err(ApiError::bad_request(format!("{s} is not covered by *.{}", b.zone)));
            }
        }
        let spec = crate::jobs::JobSpec::new("wildcard_ssl", &format!("Wildcard certificate for *.{}", b.zone)).lock("certbot").timeout(Duration::from_secs(1200)).actor(Some(addr.ip().to_canonical().to_string()));
        let st = state.clone();
        let zone = b.zone.clone();
        let email = b.email.trim().to_string();
        let sites = b.sites.clone();
        Ok(state.jobs.submit(spec, move |ctx| async move {
            let ini = cf_ini(&p.id);
            std::fs::create_dir_all("/etc/nexacloud/dns")?;
            crate::util::atomic_write(std::path::Path::new(&ini), format!("dns_cloudflare_api_token = {}\n", p.token).as_bytes(), 0o600, Some((0, 0)))?;
            if !std::path::Path::new("/usr/lib/python3/dist-packages/certbot_dns_cloudflare").exists() {
                ctx.log("Installing certbot and its Cloudflare plugin…");
                let o = Cmd::new("apt-get").args(["install", "-y", "-q", "certbot", "python3-certbot-dns-cloudflare"]).env("DEBIAN_FRONTEND", "noninteractive").timeout(Duration::from_secs(900)).run().await;
                if !o.ok {
                    anyhow::bail!("installing the certbot Cloudflare plugin failed: {}", o.error_summary("apt-get"));
                }
            }
            let name = format!("{zone}-wildcard");
            ctx.log(&format!("Requesting {zone} and *.{zone} (DNS check through Cloudflare, ~1 minute)…"));
            let mut args: Vec<String> = ["certonly", "--dns-cloudflare", "--dns-cloudflare-credentials", &ini, "--dns-cloudflare-propagation-seconds", "30", "--cert-name", &name, "--non-interactive", "--agree-tos", "--keep-until-expiring", "-m", &email, "-d", &zone, "-d", &format!("*.{zone}")].iter().map(|s| s.to_string()).collect();
            if let Ok(server) = std::env::var("NEXACLOUD_ACME_SERVER") {
                args.extend(["--server".into(), server]);
            }
            let o = Cmd::new("certbot").args(args).timeout(Duration::from_secs(900)).run().await;
            for l in o.combined().lines().filter(|l| !l.trim().is_empty()).take(40) {
                ctx.log(l);
            }
            if !o.ok {
                anyhow::bail!("certbot could not get the certificate");
            }
            let live = format!("/etc/letsencrypt/live/{name}");
            for s in sites {
                ctx.log(&format!("Using it for {s}"));
                let res = (|| -> Result<crate::changes::ChangePlan, String> {
                    let old = crate::plans::sites::load_model(&st.changes_env, &s)?;
                    let mut new = old.clone();
                    new.ssl = true;
                    new.ssl_cert = Some(format!("{live}/fullchain.pem"));
                    new.ssl_key = Some(format!("{live}/privkey.pem"));
                    crate::plans::sites::plan_use_cert(&st.changes_env, &old, new)
                })();
                match res {
                    Ok(plan) => {
                        if let Err(e) = crate::changes::apply_now(&st.db, &st.changes_env, plan, "wildcard certificate", &ctx.log).await {
                            ctx.log(&format!("  {s}: {e:#}"));
                        }
                    }
                    Err(e) => ctx.log(&format!("  {s}: {e}")),
                }
            }
            ctx.log("Done — certbot renews it automatically.");
            Ok(())
        })?)
    }
    .await;
    if let Ok(j) = &res {
        info.job_id = Some(j.clone());
    }
    audited(res.map(|j| Json(json!({"job_id": j})).into_response()).unwrap_or_else(IntoResponse::into_response), info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_records_and_builds_email_sets() {
        let r = |k: &str, n: &str, c: &str, p: Option<u16>| Record { id: String::new(), kind: k.into(), name: n.into(), content: c.into(), ttl: 3600, priority: p, proxied: None };
        assert!(validate(&r("A", "@", "203.0.113.5", None)).is_ok());
        assert!(validate(&r("A", "www", "not-an-ip", None)).is_err());
        assert!(validate(&r("MX", "@", "mx.example.com", None)).is_err(), "MX needs priority");
        assert!(validate(&r("CNAME", "a b", "x.example.com", None)).is_err());
        assert!(validate(&r("TXT", "@", "line\nbreak", None)).is_err());
        assert!(validate(&r("TXT", "*.dev", "ok", None)).is_ok());
        assert_eq!(relative("www.example.com", "example.com"), "www");
        assert_eq!(relative("example.com.", "example.com"), "@");
        let e = EmailSetup { provider: "google".into(), mx: vec![], spf_include: None, dkim_selector: Some("google".into()), dkim_value: Some("v=DKIM1; k=rsa; p=MIIB".into()), dmarc_policy: "quarantine".into(), dmarc_email: Some("dmarc@example.com".into()) };
        let v = email_records("example.com", &e).unwrap();
        assert_eq!(v[0], r("MX", "@", "smtp.google.com", Some(1)));
        assert!(v.iter().any(|x| x.name == "@" && x.content == "v=spf1 include:_spf.google.com ~all"));
        assert!(v.iter().any(|x| x.name == "google._domainkey"));
        assert!(v.iter().any(|x| x.name == "_dmarc" && x.content == "v=DMARC1; p=quarantine; rua=mailto:dmarc@example.com"));
        let m = email_records("shop.co.uk", &EmailSetup { provider: "microsoft".into(), ..e.clone() }).unwrap();
        assert_eq!(m[0].content, "shop-co-uk.mail.protection.outlook.com");
        assert!(email_records("x.com", &EmailSetup { provider: "custom".into(), mx: vec!["mx.x.net".into()], ..e }).is_err());
    }
}
