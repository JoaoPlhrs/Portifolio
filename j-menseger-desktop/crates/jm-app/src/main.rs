//! J-Menseger para computador.
//!
//! Aplicativo nativo: a interface é desenhada pela GPU (ou por software, quando
//! não há GPU), sem navegador embutido em nenhuma camada.

mod app;
mod bridge;
mod theme;

use app::{App, Msg};
use iced::{Size, Task};

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jm_app=info,jm_core=info".into()),
        )
        .init();

    let db = db_path();
    if let Some(dir) = db.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    tracing::info!("dados em {}", db.display());

    // Para testes locais: J_MENSEGER_RELAYS=ws://127.0.0.1:7781
    let relays = std::env::var("J_MENSEGER_RELAYS").ok().map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });

    iced::application(
        move || {
            let (app, task) = App::new(db.clone(), relays.clone());
            let ponte = Task::run(bridge_stream(), |ev| Msg::FromEngine(Box::new(ev)));
            (app, Task::batch([task, ponte]))
        },
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .window_size(Size::new(1180.0, 760.0))
    .run()
}

/// Fluxo com os avisos do motor, consumido pela interface.
fn bridge_stream() -> impl futures_util::Stream<Item = jm_core::engine::EngineEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    bridge::subscribe(tx);
    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (ev, rx))
    })
}

/// Onde os dados ficam, seguindo a convenção de cada sistema.
fn db_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("J_MENSEGER_DATA") {
        return std::path::PathBuf::from(p);
    }
    let base = if cfg!(windows) {
        std::env::var("APPDATA").ok().map(std::path::PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var("XDG_DATA_HOME")
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
    };
    base.unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("J-Menseger")
        .join("jmenseger.db")
}
