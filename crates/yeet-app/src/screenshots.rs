//! Renders the main screens to PNGs with the software renderer, for checking the design
//! without a display: YEET_SCREENSHOT=<dir> yeet

use crate::{AppWindow, DeviceRow, ItemRow, TransferRow};
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

const SCALE: f32 = 2.0;
const WIDTH: u32 = 480;
const HEIGHT: u32 = 720;

struct Headless {
    window: RefCell<Option<Rc<MinimalSoftwareWindow>>>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        *self.window.borrow_mut() = Some(w.clone());
        Ok(w)
    }
}

pub fn render_all(dir: PathBuf) {
    std::fs::create_dir_all(&dir).expect("create screenshot dir");
    let platform = Rc::new(Headless { window: RefCell::new(None) });
    slint::platform::set_platform(Box::new(Shared(platform.clone()))).expect("set platform");

    let scenes: [(&str, i32, fn(&AppWindow)); 7] = [
        ("empty-light", 1, empty),
        ("sending-light", 1, sending),
        ("sending-dark", 2, sending),
        ("offer-light", 1, offer),
        ("settings-dark", 2, settings),
        ("trouble-light", 1, trouble),
        ("connect-dark", 2, connect),
    ];
    for (name, scheme, setup) in scenes {
        let ui = AppWindow::new().expect("create window");
        let window = platform.window.borrow().clone().expect("window adapter");
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: SCALE });
        window.set_size(PhysicalSize::new((WIDTH as f32 * SCALE) as u32, (HEIGHT as f32 * SCALE) as u32));
        ui.set_device_name("My Desktop".into());
        ui.set_force_scheme(scheme);
        setup(&ui);
        ui.show().expect("show");
        // Let sheet and progress animations finish before capturing.
        for _ in 0..6 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(80));
        }
        slint::platform::update_timers_and_animations();
        let path = dir.join(format!("{name}.png"));
        save(&window, &path);
        println!("{}", path.display());
        ui.hide().ok();
    }
}

/// `set_platform` takes ownership; this lets us keep a handle to the current window.
struct Shared(Rc<Headless>);

impl Platform for Shared {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        self.0.create_window_adapter()
    }
}

fn save(window: &MinimalSoftwareWindow, path: &std::path::Path) {
    let size = window.size();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut buf = vec![PremultipliedRgbaColor::default(); w * h];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, w);
    });
    let mut bytes = Vec::with_capacity(w * h * 4);
    for p in &buf {
        let a = p.alpha.max(1) as u32;
        let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
        bytes.extend_from_slice(&[un(p.red), un(p.green), un(p.blue), p.alpha]);
    }
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&bytes)).expect("write png");
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn devices() -> ModelRc<DeviceRow> {
    model(vec![
        DeviceRow { name: "Gaming-Laptop".into(), detail: "192.168.1.42".into() },
        DeviceRow { name: "Living Room PC".into(), detail: "192.168.1.17".into() },
        DeviceRow { name: "MacBook Air".into(), detail: "192.168.1.88".into() },
    ])
}

fn row(
    incoming: bool,
    title: &str,
    subtitle: &str,
    progress: f32,
    detail: &str,
    state: &str,
    can_open: bool,
) -> TransferRow {
    TransferRow {
        incoming,
        title: title.into(),
        subtitle: subtitle.into(),
        progress,
        detail: detail.into(),
        state: state.into(),
        can_open,
    }
}

fn empty(ui: &AppWindow) {
    ui.set_items_summary("0 items".into());
    ui.set_defender_banner(true);
}

fn sending(ui: &AppWindow) {
    ui.set_items(model(vec![
        ItemRow { name: "BeamNG.drive".into(), size: "38.42 GB".into(), folder: true },
        ItemRow { name: "yeet".into(), size: "Calculating…".into(), folder: true },
        ItemRow { name: "notes.txt".into(), size: "4 KB".into(), folder: false },
    ]));
    ui.set_items_summary("3 items".into());
    ui.set_devices(devices());
    ui.set_selected_device(0);
    ui.set_transfers(model(vec![
        row(
            false,
            "To Gaming-Laptop",
            "Cyberpunk 2077 · 12,384 files",
            0.62,
            "41.2 GB of 66.5 GB · 112 MB/s · about 3 min 45 s left",
            "active",
            false,
        ),
        row(true, "From MacBook Air", "Photos · 1,204 files", 1.0, "8.40 GB in 1 min 16 s · 110 MB/s", "done", true),
    ]));
}

fn offer(ui: &AppWindow) {
    ui.set_devices(devices());
    ui.set_offer_from("Gaming-Laptop".into());
    ui.set_offer_summary("66,077 files · 12.40 GB".into());
    let items: Vec<SharedString> = vec!["SteamLibrary".into(), "saves".into(), "readme.md".into()];
    ui.set_offer_items(model(items));
    ui.set_offer_visible(true);
}

fn settings(ui: &AppWindow) {
    ui.set_defender_banner(true);
    ui.set_defender_busy(true);
    ui.set_devices(devices());
    ui.set_settings_name("My Desktop".into());
    ui.set_settings_folder("C:\\Users\\me\\Downloads\\yeet".into());
    ui.set_settings_addresses("192.168.1.10".into());
    ui.set_settings_compress(true);
    ui.set_settings_visible(true);
}

fn trouble(ui: &AppWindow) {
    ui.set_devices(devices());
    ui.set_transfers(model(vec![
        row(
            false,
            "To Living Room PC",
            "BeamNG.drive · 4,812 files",
            0.37,
            "Connection lost. Reconnecting… · 14.2 GB of 38.4 GB",
            "reconnecting",
            false,
        ),
        row(
            false,
            "To Gaming-Laptop",
            "3 items · 210 files",
            0.0,
            "Waiting for Gaming-Laptop to accept…",
            "waiting",
            false,
        ),
        row(
            false,
            "To MacBook Air",
            "repo · 8,021 files",
            0.12,
            "Couldn't reach MacBook Air. Check that yeet is open there and both are on the same network.",
            "failed",
            false,
        ),
    ]));
    ui.set_toast("Gaming-Laptop stopped sending.".into());
}

fn connect(ui: &AppWindow) {
    ui.set_connect_address("192.168.1.".into());
    ui.set_connect_visible(true);
}
