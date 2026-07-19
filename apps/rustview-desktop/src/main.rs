mod app;
mod identity;
mod media;
mod network;
mod platform;
mod settings;

use app::RustViewApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    if std::env::args()
        .skip(1)
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!(
            "RustView desktop\n\nUSAGE:\n  rustview-desktop\n\nENVIRONMENT:\n  RUSTVIEW_RELAY       Override the saved relay address\n  RUSTVIEW_CONFIG_DIR  Override the per-user RustView config directory"
        );
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustview=info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("RustView"),
        ..Default::default()
    };

    eframe::run_native(
        "RustView",
        options,
        Box::new(|cc| Ok(Box::new(RustViewApp::new(cc)))),
    )
}
