//! rpm-ostree client actor.

use super::cli_status::Status;
use super::Release;
use actix::prelude::*;
use anyhow::{Context, Result};
use filetime::FileTime;
use log::trace;
use std::collections::BTreeSet;
use std::rc::Rc;

/// Cache of local deployments.
#[derive(Clone, Debug)]
pub struct StatusCache {
    pub status: Rc<Status>,
    pub mtime: FileTime,
}

/// Client actor for rpm-ostree.
#[derive(Debug, Default, Clone)]
pub struct RpmOstreeClient {
    // NB: This is OK for now because `rpm-ostree` actor is curently spawned on a single thread,
    // but if we move to a larger threadpool, each actor thread will have its own cache.
    pub status_cache: Option<StatusCache>,
}

impl Actor for RpmOstreeClient {
    type Context = SyncContext<Self>;
}

impl RpmOstreeClient {
    /// Start the threadpool for rpm-ostree blocking clients.
    pub fn start(threads: usize) -> Addr<Self> {
        SyncArbiter::start(threads, RpmOstreeClient::default)
    }
}

/// Request: stage a deployment (in finalization-locked mode).
#[derive(Debug, Clone)]
pub struct StageDeployment {
    /// Whether to allow downgrades.
    pub allow_downgrade: bool,
    /// Release to be staged.
    pub release: Release,
}

impl Message for StageDeployment {
    type Result = Result<Release>;
}

impl Handler<StageDeployment> for RpmOstreeClient {
    type Result = Result<Release>;

    fn handle(&mut self, msg: StageDeployment, _ctx: &mut Self::Context) -> Self::Result {
        let booted = super::cli_status::invoke_cli_status(true)?;
        let local_deploy = super::cli_status::booted_status(&booted)?;

        let custom_origin = local_deploy.custom_origin();
        let release = if msg.release.is_oci {
            if let Some(custom_origin) = custom_origin {
                // make sure the update comes from the same origin
                let release_pullspec_split: Vec<&str> =
                    msg.release.checksum.split("@sha256:").collect();
                if !custom_origin[0].contains(release_pullspec_split[0]) {
                    anyhow::bail!(
                        "The update pullspec provided does not match local custom-origin-url",
                    );
                }

                // remove the tag from the custom origin
                // as it's a moving tag and we want to pin versions
                let origin_prefix = custom_origin[0]
                    .rsplit_once(":")
                    .map(|(origin, _)| origin)
                    .ok_or(anyhow::anyhow!("Invalid custom-origin-url format"))?;

                // append the release digest to the custom origin url
                let release_sha_digest = release_pullspec_split[1];
                let full_rebase_spec = format!("{origin_prefix}@sha256:{release_sha_digest}");

                // re-craft a release object with the pullspec
                let oci_release = Release {
                    version: msg.release.version.clone(),
                    checksum: full_rebase_spec,
                    age_index: msg.release.age_index,
                    is_oci: true,
                };
                trace!("request to stage release: {:?}", oci_release);
                super::cli_deploy::deploy_locked(
                    oci_release,
                    msg.allow_downgrade,
                    Some(custom_origin),
                )
            } else {
                anyhow::bail!("Zincati does not support OCI updates if rpm-ostree custom origin is not set on local deployement.");
            }
        } else {
            trace!("request to stage release: {:?}", msg.release);
            super::cli_deploy::deploy_locked(msg.release, msg.allow_downgrade, None)
        };
        trace!("rpm-ostree CLI returned: {:?}", release);
        release
    }
}

/// Request: finalize a staged deployment (by unlocking it and rebooting).
#[derive(Debug, Clone)]
pub struct FinalizeDeployment {
    /// Finalized release to finalize.
    pub release: Release,
}

impl Message for FinalizeDeployment {
    type Result = Result<Release>;
}

impl Handler<FinalizeDeployment> for RpmOstreeClient {
    type Result = Result<Release>;

    fn handle(&mut self, msg: FinalizeDeployment, _ctx: &mut Self::Context) -> Self::Result {
        trace!("request to finalize release: {:?}", msg.release);
        let release = super::cli_finalize::finalize_deployment(msg.release);
        trace!("rpm-ostree CLI returned: {:?}", release);
        release
    }
}

/// Request: query local deployments.
#[derive(Debug, Clone)]
pub struct QueryLocalDeployments {
    /// Whether to include staged (i.e. not finalized) deployments in query result.
    pub(crate) omit_staged: bool,
}

impl Message for QueryLocalDeployments {
    type Result = Result<BTreeSet<Release>>;
}

impl Handler<QueryLocalDeployments> for RpmOstreeClient {
    type Result = Result<BTreeSet<Release>>;

    fn handle(
        &mut self,
        query_msg: QueryLocalDeployments,
        _ctx: &mut Self::Context,
    ) -> Self::Result {
        trace!("request to list local deployments");
        let releases = super::cli_status::local_deployments(self, query_msg.omit_staged);
        trace!("rpm-ostree CLI returned: {:?}", releases);
        releases
    }
}

/// Request: query pending deployment and stream.
#[derive(Debug, Clone)]
pub struct QueryPendingDeploymentStream {}

impl Message for QueryPendingDeploymentStream {
    type Result = Result<Option<(Release, String)>>;
}

impl Handler<QueryPendingDeploymentStream> for RpmOstreeClient {
    type Result = Result<Option<(Release, String)>>;

    fn handle(
        &mut self,
        _msg: QueryPendingDeploymentStream,
        _ctx: &mut Self::Context,
    ) -> Self::Result {
        trace!("fetching details for staged deployment");

        let status = super::cli_status::invoke_cli_status(false)?;
        super::cli_status::parse_pending_deployment(&status)
            .context("failed to introspect pending deployment")
    }
}

/// Request: cleanup pending deployment.
#[derive(Debug, Clone)]
pub struct CleanupPendingDeployment {}

impl Message for CleanupPendingDeployment {
    type Result = Result<()>;
}

impl Handler<CleanupPendingDeployment> for RpmOstreeClient {
    type Result = Result<()>;

    fn handle(&mut self, _msg: CleanupPendingDeployment, _ctx: &mut Self::Context) -> Self::Result {
        trace!("request to cleanup pending deployment");
        super::cli_deploy::invoke_cli_cleanup()?;
        Ok(())
    }
}

/// Request: Register as the update driver for rpm-ostree.
#[derive(Debug, Clone)]
pub struct RegisterAsDriver {}

impl Message for RegisterAsDriver {
    type Result = ();
}

impl Handler<RegisterAsDriver> for RpmOstreeClient {
    type Result = ();

    fn handle(&mut self, _msg: RegisterAsDriver, _ctx: &mut Self::Context) -> Self::Result {
        trace!("request to register as rpm-ostree update driver");
        super::cli_deploy::deploy_register_driver()
    }
}
