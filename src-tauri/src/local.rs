use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{async_runtime, AppHandle, Manager, Runtime};
use tauri_plugin_pinia::ManagerExt;
use tauri_specta::Event;
use tokio::{
    select,
    sync::mpsc,
    time::{self, Instant},
};
use tpower::{
    ffi::smc::{SMCConnection, SMCReadSensor},
    provider::{get_mac_ioreg, NormalizedResource},
};

use crate::event::{PowerUpdatedEvent, PreferenceEvent, StatusBarItem, WindowLoadedEvent};

pub enum SenderMessage {
    ImmediateSend,
    ChangeInterval(Duration),
    ChangeStatusBarItem(StatusBarItem),
    StatusBarShowCharging(bool),
}

/// Bounds for the power-tick interval. A stale or corrupt preference must not
/// stall the chart (e.g. a persisted `updateInterval` of one day).
const MIN_INTERVAL: Duration = Duration::from_millis(500);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const SMC_RETRY_INTERVAL: Duration = Duration::from_secs(30);

fn make_interval(period: Duration) -> time::Interval {
    let period = period.clamp(MIN_INTERVAL, MAX_INTERVAL);
    let mut timer = time::interval(period);
    timer.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    timer
}

pub fn status_bar_value(
    data: &NormalizedResource,
    status_bar_item: &StatusBarItem,
    show_charging: bool,
) -> f32 {
    if data.is_charging && show_charging {
        return data.system_in;
    }
    match status_bar_item {
        StatusBarItem::System => data.system_load,
        StatusBarItem::Screen => data.brightness_power,
        StatusBarItem::Heatpipe => data.heatpipe_power,
    }
}

impl PowerUpdatedEvent {
    /// Right-align the number with figure spaces (same width as a digit) so the
    /// menu bar item keeps a constant width.
    pub fn new(value: f32) -> Self {
        let text = format!("{value:.1}");
        let pad = 4usize.saturating_sub(text.len());
        Self(format!("{}{text} w", "\u{2007}".repeat(pad)))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Event, Type)]
#[serde(rename_all = "camelCase")]
pub struct PowerTickEvent {
    pub data: NormalizedResource,
}

struct Sampler {
    smc: Option<SMCConnection>,
    next_smc_retry: Instant,
    last: Option<NormalizedResource>,
}

impl Sampler {
    fn new() -> Self {
        let smc = SMCConnection::new("AppleSMC")
            .inspect_err(|e| log::warn!("AppleSMC unavailable ({e}), using IORegistry only"))
            .ok();
        Self {
            smc,
            next_smc_retry: Instant::now() + SMC_RETRY_INTERVAL,
            last: None,
        }
    }

    /// Read a local sample. Never panics: falls back to IORegistry-only data
    /// when the SMC is unavailable, and to SMC-only data when there is no
    /// `AppleSmartBattery` (desktop Macs) or it cannot be parsed.
    fn sample(&mut self) -> Option<NormalizedResource> {
        if self.smc.is_none() && Instant::now() >= self.next_smc_retry {
            match SMCConnection::new("AppleSMC") {
                Ok(conn) => self.smc = Some(conn),
                Err(e) => {
                    log::warn!("AppleSMC still unavailable ({e})");
                    self.next_smc_retry = Instant::now() + SMC_RETRY_INTERVAL;
                }
            }
        }

        let smc = self.smc.as_mut().map(SMCReadSensor::read_sensor);
        let data = match (get_mac_ioreg(), smc) {
            (Ok(ioreg), Some(smc)) => (&ioreg, &smc).into(),
            (Ok(ioreg), None) => NormalizedResource {
                is_local: true,
                ..NormalizedResource::from(&ioreg)
            },
            (Err(e), Some(smc)) => {
                log::debug!("IORegistry unavailable, using SMC only: {e:#}");
                NormalizedResource::from(&smc)
            }
            (Err(e), None) => {
                log::error!("no power data source available: {e:#}");
                return None;
            }
        };
        self.last = Some(data.clone());
        Some(data)
    }
}

fn emit_power_sample<R: Runtime>(
    app: &AppHandle<R>,
    data: Option<&NormalizedResource>,
    status_bar_item: &StatusBarItem,
    show_charging: bool,
    tick: bool,
) {
    let Some(data) = data else { return };
    if let Err(e) =
        PowerUpdatedEvent::new(status_bar_value(data, status_bar_item, show_charging)).emit(app)
    {
        log::error!("failed to emit PowerUpdatedEvent: {e}");
    }
    if tick {
        if let Err(e) = (PowerTickEvent { data: data.clone() }).emit(app) {
            log::error!("failed to emit PowerTickEvent: {e}");
        }
    }
}

pub fn start_sender<R: Runtime>(
    app: &impl Manager<R>,
    mut rx: mpsc::UnboundedReceiver<SenderMessage>,
) -> async_runtime::JoinHandle<()> {
    let app = app.app_handle().clone();
    let mut sampler = Sampler::new();

    let mut timer = make_interval(Duration::from_millis(
        app.pinia()
            .try_get::<u64>("preference", "updateInterval")
            .unwrap_or(2000),
    ));
    let mut status_bar_item = app
        .pinia()
        .try_get::<StatusBarItem>("preference", "statusBarItem")
        .unwrap_or_default();
    let mut show_charging = app
        .pinia()
        .try_get::<bool>("preference", "statusBarShowCharging")
        .unwrap_or(true);

    async_runtime::spawn(async move {
        loop {
            select! {
                _ = timer.tick() => {
                    let data = sampler.sample();
                    emit_power_sample(&app, data.as_ref(), &status_bar_item, show_charging, true);
                }
                Some(msg) = rx.recv() => match msg {
                    SenderMessage::ImmediateSend => {
                        let data = sampler.sample();
                        emit_power_sample(&app, data.as_ref(), &status_bar_item, show_charging, true);
                    },
                    SenderMessage::ChangeInterval(interval) => {
                        timer = make_interval(interval);
                    },
                    SenderMessage::ChangeStatusBarItem(item) => {
                        status_bar_item = item;
                        emit_power_sample(&app, sampler.last.as_ref(), &status_bar_item, show_charging, false);
                    },
                    SenderMessage::StatusBarShowCharging(show) => {
                        show_charging = show;
                        emit_power_sample(&app, sampler.last.as_ref(), &status_bar_item, show_charging, false);
                    }
                }
            }
        }
    })
}

pub fn setup_sender_with_events<R: Runtime>(app: &impl Manager<R>) {
    let app = app.app_handle();
    let (sender_tx, rx) = mpsc::unbounded_channel();
    start_sender(app, rx);

    // send an immediate update when the main window is loaded
    let tx = sender_tx.clone();
    WindowLoadedEvent::listen(app, move |_| {
        if let Err(e) = tx.send(SenderMessage::ImmediateSend) {
            log::error!("failed to request immediate power update: {e}");
        }
    });

    let tx = sender_tx;
    PreferenceEvent::listen(app, move |event| {
        let msg = match event.payload {
            PreferenceEvent::UpdateInterval(interval) => Some(SenderMessage::ChangeInterval(
                Duration::from_millis(interval.into()),
            )),
            PreferenceEvent::StatusBarItem(item) => Some(SenderMessage::ChangeStatusBarItem(item)),
            PreferenceEvent::StatusBarShowCharging(show) => {
                Some(SenderMessage::StatusBarShowCharging(show))
            }
            _ => None,
        };
        if let Some(msg) = msg {
            if let Err(e) = tx.send(msg) {
                log::error!("failed to apply preference update: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_text_has_constant_width() {
        assert_eq!(PowerUpdatedEvent::new(3.3).0, "\u{2007}3.3 w");
        assert_eq!(PowerUpdatedEvent::new(12.0).0, "12.0 w");
    }

    #[tokio::test]
    async fn interval_is_clamped() {
        assert_eq!(make_interval(Duration::from_millis(1)).period(), MIN_INTERVAL);
        assert_eq!(make_interval(Duration::from_secs(86_400)).period(), MAX_INTERVAL);
    }
}
