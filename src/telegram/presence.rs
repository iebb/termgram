//! Reports the status decided by `crate::presence` with
//! `account.updateStatus`, repeating it while online as Telegram Desktop's
//! `Api::Updates::updateOnline` does. Nothing is sent until the application
//! first asks to be online.

use super::{Client, tl};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, sync::watch, time::Instant};

/// Desktop's default `onlineUpdatePeriod`, used when the server sends none.
const UPDATE_PERIOD: Duration = Duration::from_secs(120);
const RETRY: Duration = Duration::from_secs(5);

/// Shared between the worker, which records what is wanted and observed, and
/// the task that talks to Telegram.
#[derive(Clone, Default)]
pub(super) struct Handle {
    pub(super) wanted: watch::Sender<bool>,
    /// Another session went offline, which also hides this one.
    pub(super) other_offline: Arc<Notify>,
    /// The status Telegram last acknowledged.
    online: Arc<AtomicBool>,
}

impl Handle {
    /// A status update about this account, sent by any of its sessions.
    pub(super) fn own_status(&self, status: &tl::enums::UserStatus) {
        if matches!(status, tl::enums::UserStatus::Offline(_)) {
            self.other_offline.notify_one();
        }
    }

    /// Leave no stale "online" behind when the worker stops.
    pub(super) async fn finish(&self, client: &Client) {
        if self.online.swap(false, Ordering::AcqRel) {
            let request = tl::functions::account::UpdateStatus { offline: true };
            let _ = tokio::time::timeout(Duration::from_secs(1), client.invoke(&request)).await;
        }
    }
}

pub(super) async fn report(client: Client, handle: Handle) {
    let mut wanted = handle.wanted.subscribe();
    let mut period = None;
    let mut sent_at = None::<Instant>;
    let mut retry_at = None::<Instant>;
    loop {
        let target = *wanted.borrow_and_update();
        let online = handle.online.load(Ordering::Acquire);
        let refresh_at = sent_at.zip(period).map(|(sent, period)| sent + period);
        let due = target != online
            || (target && refresh_at.is_none_or(|refresh| Instant::now() >= refresh));
        if due && retry_at.is_none_or(|retry| Instant::now() >= retry) {
            if target && period.is_none() {
                period = Some(update_period(&client).await);
            }
            let request = tl::functions::account::UpdateStatus { offline: !target };
            if client.invoke(&request).await.is_ok() {
                handle.online.store(target, Ordering::Release);
                sent_at = Some(Instant::now());
                retry_at = None;
            } else {
                retry_at = Some(Instant::now() + RETRY);
            }
            continue;
        }
        let wake = retry_at.or(refresh_at.filter(|_| target));
        tokio::select! {
            changed = wanted.changed() => { if changed.is_err() { return; } },
            () = handle.other_offline.notified() => {
                // Telegram now shows the account offline; say so again if it is not.
                if target { sent_at = None; }
            },
            () = async {
                match wake {
                    Some(wake) => tokio::time::sleep_until(wake).await,
                    None => std::future::pending().await,
                }
            } => {},
        }
    }
}

async fn update_period(client: &Client) -> Duration {
    match client.invoke(&tl::functions::help::GetConfig {}).await {
        Ok(tl::enums::Config::Config(config)) => u64::try_from(config.online_update_period_ms)
            .ok()
            .filter(|millis| *millis >= 1_000)
            .map_or(UPDATE_PERIOD, Duration::from_millis),
        Err(_) => UPDATE_PERIOD,
    }
}
