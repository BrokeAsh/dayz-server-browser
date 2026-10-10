//! A DayZ server browser and launcher that works with the DayZ map overlay.

// No console window behind the browser on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod a2s;
mod address;
mod dzsa;
mod http;
mod launch;
mod mods;
mod overlay;
mod secret;
mod server;
mod settings;
mod steam;
mod steam_api;
mod ui;
mod update;
mod vdf;
mod version;
#[cfg(windows)]
mod win;
mod workshop;

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("dayz-browser {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,wgpu=warn,wgpu_hal=warn,egui_wgpu=warn,zbus=warn,tracing=warn,naga=warn",
    ))
    .init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("DayZ Server Browser")
            .with_app_id("dayz-browser")
            .with_inner_size([1200.0, 720.0])
            .with_min_inner_size([900.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DayZ Server Browser",
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
