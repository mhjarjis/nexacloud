//! Test-only endpoints, mounted **only** when the agent starts with
//! `NEXACLOUD_TEST_HOOKS=1` (the Docker e2e test sets it through a systemd
//! drop-in inside its throwaway container). Never enabled in normal runs;
//! without the variable these paths are plain 404s. They still require a
//! session and sudo mode, and take no paths or commands from the client.

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::changes::{self, ChangePlan};
use crate::error::{ApiError, ApiResult};
use crate::jobs::JobSpec;
use crate::routes::changes::client_ip;
use crate::state::AppState;
use crate::util::cmd::Cmd;

const BROKEN_DROPIN: &str = "/etc/nginx/conf.d/zz-nexacloud-test-broken.conf";

/// `POST /api/test-hooks/broken-change`: a change whose `nginx -t` fails, to
/// prove the automatic revert. Fixed payload.
pub async fn broken_change(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let plan = ChangePlan::builder(
        "test_broken",
        "Test: deliberately broken nginx change",
        "nginx",
    )
    .lock("nginx")
    .write_file(BROKEN_DROPIN, "this_is_not_an_nginx_directive on;\n")
    .command(["nginx", "-t"])
    .command(["systemctl", "reload", "nginx"])
    .on_revert(["systemctl", "reload", "nginx"])
    .build(&state.changes_env)?;
    let (change_id, job_id) = changes::submit_apply(
        &state.jobs,
        &state.db,
        &state.changes_env,
        plan,
        Some(client_ip(&addr)),
    )?;
    Ok(Json(json!({"change_id": change_id, "job_id": job_id})))
}

#[derive(Deserialize)]
struct SleepBody {
    secs: u32,
    #[serde(default)]
    heavy: bool,
}

/// `POST /api/test-hooks/sleep-job {"secs": 1..600, "heavy": bool}`: a job
/// that prints `tick N` once a second (queue, cancel, SSE and restart tests).
pub async fn sleep_job(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let b: SleepBody = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("expected {\"secs\": n, \"heavy\": bool}"))?;
    if !(1..=600).contains(&b.secs) {
        return Err(ApiError::bad_request("secs must be 1..600"));
    }
    let mut spec = JobSpec::new("test_sleep", &format!("Test: sleep {}s", b.secs))
        .timeout(Duration::from_secs(900))
        .actor(Some(client_ip(&addr)));
    if b.heavy {
        spec = spec.heavy();
    }
    let secs = b.secs;
    let id = state.jobs.submit(spec, move |ctx| async move {
        let script = format!("for i in $(seq 1 {secs}); do echo \"tick $i\"; sleep 1; done");
        let out = Cmd::new("sh")
            .args(["-c", &script])
            .timeout(Duration::from_secs(u64::from(secs) + 30))
            .run_streaming(&*ctx.log)
            .await;
        anyhow::ensure!(out.ok, "sleep script failed");
        Ok(())
    })?;
    Ok(Json(json!({"job_id": id})))
}
