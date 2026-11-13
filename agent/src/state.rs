use std::ops::Deref;
use std::sync::Arc;

use crate::auth::{Sessions, Throttle};
use crate::config::Config;
use crate::jobs::Jobs;
use crate::routes::system::CpuSampler;

pub struct Inner {
    pub config: Config,
    pub sessions: Sessions,
    pub throttle: Throttle,
    pub jobs: Jobs,
    pub cpu: CpuSampler,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(config: Config) -> Self {
        AppState(Arc::new(Inner {
            config,
            sessions: Sessions::default(),
            throttle: Throttle::default(),
            jobs: Jobs::default(),
            cpu: CpuSampler::default(),
        }))
    }
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}
