use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;

use crate::auth::{Sessions, Throttle};
use crate::changes::ChangeEnv;
use crate::config::Config;
use crate::db::Db;
use crate::jobs::Jobs;
use crate::routes::system::CpuSampler;

pub struct Inner {
    pub config: Config,
    pub sessions: Sessions,
    pub throttle: Throttle,
    pub db: Db,
    pub jobs: Arc<Jobs>,
    /// Real filesystem root + command runner for the change pipeline.
    pub changes_env: ChangeEnv,
    pub cpu: CpuSampler,
    /// `NEXACLOUD_TEST_HOOKS=1` at startup (e2e tests only).
    pub test_hooks: bool,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    /// Create the data dirs, open the state database and build the state.
    /// Does not touch job/change statuses (see `serve` for the startup sweep).
    pub fn new(config: Config) -> Result<Self> {
        config.ensure_data_dirs()?;
        let db = Db::open(&config.state_db_path())?;
        let jobs = Jobs::new(db.clone(), config.jobs_dir());
        let changes_env = ChangeEnv::system(config.snapshots_dir());
        Ok(AppState(Arc::new(Inner {
            config,
            sessions: Sessions::default(),
            throttle: Throttle::default(),
            db,
            jobs,
            changes_env,
            cpu: CpuSampler::default(),
            test_hooks: std::env::var("NEXACLOUD_TEST_HOOKS").as_deref() == Ok("1"),
        })))
    }
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}
