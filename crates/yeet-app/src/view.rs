//! Mirrors [`State`] into the window. Runs on the UI thread on a timer.

use crate::format;
use crate::state::{App, State, Transfer};
use crate::text;
use crate::{AppWindow, DeviceRow, ItemRow, TransferRow};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

/// How long a toast stays up.
const TOAST_FOR: Duration = Duration::from_secs(4);

/// UI models kept for the window's lifetime and updated row by row, so unchanged rows aren't
/// rebuilt (which would reset hover state and animations).
pub struct Models {
    items: Rc<VecModel<ItemRow>>,
    devices: Rc<VecModel<DeviceRow>>,
    transfers: Rc<VecModel<TransferRow>>,
    offer_items: Rc<VecModel<SharedString>>,
}

impl Models {
    pub fn new(ui: &AppWindow) -> Models {
        let m = Models {
            items: Rc::default(),
            devices: Rc::default(),
            transfers: Rc::default(),
            offer_items: Rc::default(),
        };
        ui.set_items(ModelRc::from(m.items.clone()));
        ui.set_devices(ModelRc::from(m.devices.clone()));
        ui.set_transfers(ModelRc::from(m.transfers.clone()));
        ui.set_offer_items(ModelRc::from(m.offer_items.clone()));
        m
    }
}

/// Updates the window from the app state. Returns whether any transfer is running.
pub fn refresh(ui: &AppWindow, app: &App, m: &Models) -> bool {
    let mut st = app.state();
    ui.set_device_name(st.settings.device_name.clone().into());
    show_items(ui, &st, m);
    show_devices(ui, &st, m);
    show_offer(ui, &st, m);

    let rows: Vec<TransferRow> = st.transfers.iter_mut().map(transfer_row).collect();
    sync(&m.transfers, rows);
    let busy = st.transfers.iter().any(Transfer::running);

    if st.toast.as_ref().is_some_and(|(_, shown)| shown.elapsed() > TOAST_FOR) {
        st.toast = None;
    }
    ui.set_toast(st.toast.as_ref().map_or(String::new(), |(s, _)| s.clone()).into());
    let excluded = st.settings.defender_excluded.contains(&st.settings.save_dir);
    ui.set_defender_banner((st.defender && !excluded) || st.short_names);
    ui.set_defender_busy(st.defender_busy);
    busy
}

/// Replaces only the rows that changed.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

fn show_items(ui: &AppWindow, st: &State, m: &Models) {
    let rows = st
        .items
        .iter()
        .map(|it| ItemRow {
            name: text::file_name(&it.path).into(),
            size: it.size.map_or("Calculating…".into(), format::bytes).into(),
            folder: it.folder,
        })
        .collect();
    sync(&m.items, rows);
    let count = match st.items.len() {
        1 => "1 item".to_string(),
        n => format!("{n} items"),
    };
    let total: Option<u64> = st.items.iter().map(|i| i.size).sum();
    let summary = match total {
        Some(t) => format!("{count} · {}", format::bytes(t)),
        None => count,
    };
    ui.set_items_summary(summary.into());
}

fn show_devices(ui: &AppWindow, st: &State, m: &Models) {
    let rows = st
        .devices
        .iter()
        .map(|d| DeviceRow {
            name: d.peer.name.clone().into(),
            detail: if d.manual {
                "Added by address".into()
            } else {
                d.peer.addrs.first().map_or(String::new(), |a| a.ip().to_string()).into()
            },
        })
        .collect();
    sync(&m.devices, rows);
    let selected = st.selected.as_ref().and_then(|id| st.devices.iter().position(|d| &d.peer.id == id));
    ui.set_selected_device(selected.map_or(-1, |i| i as i32));
}

fn show_offer(ui: &AppWindow, st: &State, m: &Models) {
    match &st.offer {
        Some(o) => {
            ui.set_offer_from(o.from.clone().into());
            ui.set_offer_summary(o.summary.clone().into());
            sync(&m.offer_items, o.items.iter().map(SharedString::from).collect());
            ui.set_offer_visible(true);
        }
        None => ui.set_offer_visible(false),
    }
}

fn transfer_row(t: &mut Transfer) -> TransferRow {
    let live = &t.live;
    let total = live.bytes_total.load(Relaxed);
    let done = live.bytes_done.load(Relaxed);
    let counting = live.accepted.load(Relaxed) && !live.reconnecting.load(Relaxed);
    let reading = t.meter.sample(done, live.wire_bytes.load(Relaxed), total, counting);
    let (state, detail) = match &t.outcome {
        Some(Ok(r)) => {
            let mut d = format!("{} in {}", format::bytes(r.bytes), format::duration(r.secs));
            if r.secs > 0.5 {
                d += &format!(" · {}", format::rate(r.bytes as f64 / r.secs));
            }
            // Only worth mentioning when compression saved a noticeable amount.
            if !t.incoming && r.wire > 0 && (r.wire as f64) < r.bytes as f64 * 0.9 {
                d += &format!(" · {:.0}% less data sent", (1.0 - r.wire as f64 / r.bytes as f64) * 100.0);
            }
            ("done", d)
        }
        Some(Err(e)) => ("failed", e.clone()),
        None if live.reconnecting.load(Relaxed) => (
            "reconnecting",
            format!("Connection lost. Reconnecting… · {} of {}", format::bytes(done), format::bytes(total)),
        ),
        None if !live.accepted.load(Relaxed) => ("waiting", format!("Waiting for {} to accept…", t.peer)),
        None => {
            let mut d = format!("{} of {}", format::bytes(done), format::bytes(total));
            if reading.stalled {
                d += " · Waiting for data…";
            } else {
                if reading.speed > 0.0 {
                    d += &format!(" · {}", format::rate(reading.speed));
                    // Compression makes the two differ; only worth showing when they do.
                    if reading.link > 0.0 && (reading.link - reading.speed).abs() > reading.speed * 0.1 {
                        d += &format!(" ({} link)", format::rate(reading.link));
                    }
                }
                if let Some(left) = reading.left.filter(|&s| s >= 1.0) {
                    d += &format!(" · about {} left", format::duration(left));
                }
            }
            ("active", d)
        }
    };
    TransferRow {
        incoming: t.incoming,
        title: if t.incoming { format!("From {}", t.peer) } else { format!("To {}", t.peer) }.into(),
        subtitle: format!("{} · {}", t.what, format::files(live.files_total.load(Relaxed))).into(),
        progress: if total == 0 { 1.0 } else { done as f32 / total as f32 },
        detail: detail.into(),
        state: state.into(),
        can_open: t.incoming && !t.saved.is_empty() && matches!(t.outcome, Some(Ok(_))),
    }
}
