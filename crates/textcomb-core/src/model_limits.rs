use crate::{
    db,
    error::{CoreError, CoreResult, ErrorCode},
};
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) struct ProfileLimit {
    pool: PgPool,
    profile_id: Uuid,
    user_id: Uuid,
    in_flight: AtomicUsize,
    changed: Notify,
}

pub(crate) struct ProfilePermit {
    limit: Arc<ProfileLimit>,
}

impl Drop for ProfilePermit {
    fn drop(&mut self) {
        self.limit.in_flight.fetch_sub(1, Ordering::Release);
        self.limit.changed.notify_waiters();
    }
}

impl ProfileLimit {
    pub(crate) fn new(pool: PgPool, profile_id: Uuid, user_id: Uuid) -> Self {
        Self {
            pool,
            profile_id,
            user_id,
            in_flight: AtomicUsize::new(0),
            changed: Notify::new(),
        }
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        stop: &CancellationToken,
    ) -> CoreResult<ProfilePermit> {
        loop {
            // Register before reading state so a release cannot be missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let maximum = tokio::select! {
                biased;
                _ = stop.cancelled() => return Err(CoreError::public(ErrorCode::Conflict, "模型调用已停止")),
                result = db::load_model_concurrency(&self.pool, self.profile_id, self.user_id) => result?,
            };
            if self
                .in_flight
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < maximum).then_some(count + 1)
                })
                .is_ok()
            {
                return Ok(ProfilePermit {
                    limit: self.clone(),
                });
            }
            // A configuration change has no local notification; poll while
            // waiting. Lower limits drain existing permits instead of replacing
            // the gate and allowing old/new requests to exceed the shared quota.
            tokio::select! {
                _ = stop.cancelled() => return Err(CoreError::public(ErrorCode::Conflict, "模型调用已停止")),
                _ = &mut changed => {},
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    pub(crate) async fn acquire_for_call(
        self: &Arc<Self>,
        global: &Arc<Semaphore>,
        stop: &CancellationToken,
    ) -> CoreResult<(ProfilePermit, OwnedSemaphorePermit)> {
        loop {
            let profile_permit = self.acquire(stop).await?;
            let global_permit = tokio::select! {
                biased;
                _ = stop.cancelled() => return Err(CoreError::public(ErrorCode::Conflict, "模型调用已停止")),
                result = global.clone().acquire_owned() => result.map_err(|_| CoreError::public(ErrorCode::ProviderUnavailable, "模型并发控制器已关闭"))?,
            };
            // The profile limit may have decreased while waiting for the global
            // slot. Recheck before starting a call, releasing both reservations
            // on rejection so a throttled profile cannot occupy global capacity.
            let maximum = tokio::select! {
                biased;
                _ = stop.cancelled() => return Err(CoreError::public(ErrorCode::Conflict, "模型调用已停止")),
                result = db::load_model_concurrency(&self.pool, self.profile_id, self.user_id) => result?,
            };
            if self.in_flight.load(Ordering::Acquire) <= maximum {
                return Ok((profile_permit, global_permit));
            }
        }
    }
}
