//! Connects the window's callbacks to [`actions`](crate::actions).

use crate::actions;
use crate::platform;
use crate::state::App;
use crate::AppWindow;
use slint::ComponentHandle;
use std::sync::Arc;

pub fn wire(ui: &AppWindow, app: &Arc<App>) {
    wire_items(ui, app);
    wire_transfers(ui, app);
    wire_settings(ui, app);

    let a = app.clone();
    ui.on_select_device(move |i| {
        let mut st = a.state();
        st.selected = st.devices.get(i as usize).map(|d| d.peer.id.clone());
    });
    let a = app.clone();
    ui.on_send(move || actions::start_send(&a));
    let a = app.clone();
    ui.on_send_here(move || {
        if let Some(dest) = rfd::FileDialog::new().set_title("Copy to").pick_folder() {
            actions::copy_here(&a, dest);
        }
    });
    let a = app.clone();
    ui.on_accept_offer(move || actions::answer_offer(&a, true));
    let a = app.clone();
    ui.on_decline_offer(move || actions::answer_offer(&a, false));
    let a = app.clone();
    ui.on_exclude_defender(move || actions::exclude_defender(&a));
    let (a, weak) = (app.clone(), ui.as_weak());
    ui.on_connect_manual(move |text| {
        if actions::connect_manual(&a, &text) {
            if let Some(ui) = weak.upgrade() {
                ui.set_connect_visible(false);
            }
        }
    });
}

fn wire_items(ui: &AppWindow, app: &Arc<App>) {
    let a = app.clone();
    ui.on_add_files(move || {
        if let Some(files) = rfd::FileDialog::new().set_title("Choose files to send").pick_files() {
            actions::add_items(&a, files);
        }
    });
    let a = app.clone();
    ui.on_add_folder(move || {
        if let Some(folders) = rfd::FileDialog::new().set_title("Choose a folder to send").pick_folders() {
            actions::add_items(&a, folders);
        }
    });
    let a = app.clone();
    ui.on_remove_item(move |i| {
        let mut st = a.state();
        if (i as usize) < st.items.len() {
            st.items.remove(i as usize);
            actions::reprepare(&mut st);
        }
    });
    let a = app.clone();
    ui.on_clear_items(move || {
        let mut st = a.state();
        st.items.clear();
        st.prepared = None;
    });
}

fn wire_transfers(ui: &AppWindow, app: &Arc<App>) {
    let a = app.clone();
    ui.on_cancel_transfer(move |i| {
        if let Some(t) = a.state().transfers.get(i as usize) {
            t.live.request_cancel();
        }
    });
    let a = app.clone();
    ui.on_dismiss_transfer(move |i| {
        let mut st = a.state();
        if st.transfers.get(i as usize).is_some_and(|t| !t.running()) {
            st.transfers.remove(i as usize);
        }
    });
    let a = app.clone();
    ui.on_open_transfer(move |i| {
        if let Some(p) = a.state().transfers.get(i as usize).and_then(|t| t.saved.first()) {
            platform::reveal(p);
        }
    });
}

fn wire_settings(ui: &AppWindow, app: &Arc<App>) {
    let (a, weak) = (app.clone(), ui.as_weak());
    ui.on_open_settings(move || {
        let Some(ui) = weak.upgrade() else { return };
        {
            let st = a.state();
            ui.set_settings_name(st.settings.device_name.clone().into());
            ui.set_settings_folder(st.settings.save_dir.display().to_string().into());
            ui.set_settings_compress(st.settings.compression);
        }
        ui.set_settings_addresses(actions::own_address(&a).into());
        ui.set_settings_visible(true);
    });
    let (a, weak) = (app.clone(), ui.as_weak());
    ui.on_choose_folder(move || {
        let Some(ui) = weak.upgrade() else { return };
        if let Some(dir) = rfd::FileDialog::new().set_title("Save received files to").pick_folder() {
            ui.set_settings_folder(dir.display().to_string().into());
            yeet_core::disk::prewarm(vec![dir.clone()]);
            a.state().settings.save_dir = dir;
        }
    });
    let (a, weak) = (app.clone(), ui.as_weak());
    ui.on_save_settings(move || {
        let Some(ui) = weak.upgrade() else { return };
        let name = ui.get_settings_name().trim().to_string();
        let mut st = a.state();
        if !name.is_empty() && name != st.settings.device_name {
            st.settings.device_name = name.clone();
            if let Some(d) = a.discovery.lock().expect("poisoned").as_ref() {
                let _ = d.set_name(&name);
            }
        }
        st.settings.compression = ui.get_settings_compress();
        st.settings.save();
        ui.set_settings_visible(false);
    });
}
