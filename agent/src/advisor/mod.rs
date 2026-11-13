//! Server advisor: checks with one-click fixes. Every fix is a
//! [`ChangePlan`](crate::changes::ChangePlan) that goes through the change
//! pipeline (preview → snapshot → apply → verify → auto-revert).

pub mod nginx_hardening;
pub mod rules;
