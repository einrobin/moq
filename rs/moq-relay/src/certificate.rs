use futures::FutureExt;
use moq_native::{ServerReloader, ServerTlsConfig};
use notify::{EventKind, RecursiveMode};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use std::sync::Arc;

async fn watch_files_debounced<F>(paths: Vec<PathBuf>, mut on_reload: F) -> notify::Result<()>
where
    F: FnMut() -> futures::future::BoxFuture<'static, ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify_debouncer_full::new_debouncer(Duration::from_secs(5), None, tx)?;

    println!("Starting listening");
    println!("{:?}", paths);

    for path in paths {
        watcher.watch(path, RecursiveMode::NonRecursive)?;
    }

    // Blocking loop listening for events
    println!("Listening");
    for event_result in rx {
        match event_result {
            Ok(events) => {
                println!("Receive event");
                for event in events {
                    println!("{:?}", event.kind);
                    // Only handle modify or create
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        on_reload().await;
                    }
                }
            }
            Err(errors) => {
                for err in errors {
                    tracing::warn!(%err, "certificate watcher error");
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn watch_server_certificates(reloader: ServerReloader, tls_config: ServerTlsConfig) {
    if tls_config.cert.is_empty() {
        return;
    }

    let paths: Vec<PathBuf> = tls_config.cert.iter().chain(&tls_config.key).cloned().collect();

    let reloader = Arc::new(reloader);
    let tls_config = Arc::new(tls_config);

    tokio::spawn(async move {
        let result = watch_files_debounced(paths, move || {
            let reloader = reloader.clone();
            let tls_config = tls_config.clone();

            async move {
                tracing::info!("reloading server certificate");
                if let Err(err) = reloader.reload(tls_config.as_ref()) {
                    tracing::warn!(%err, "failed to reload server certificate");
                }
            }
                .boxed()
        })
            .await;

        if let Err(err) = result {
            tracing::warn!(%err, "failed to set up server certificate watcher, certificates will not be updated without a restart");
        }
    });
}

pub(crate) fn watch_web_certificate(
    config: hyper_serve::tls_rustls::RustlsConfig,
    cert: PathBuf,
    key: PathBuf,
) {
    let paths = vec![cert.clone(), key.clone()];

    let config = Arc::new(config);
    let cert = Arc::new(cert);
    let key = Arc::new(key);

    tokio::spawn(async move {
        let result = watch_files_debounced(paths, move || {
            let config = config.clone();
            let cert = cert.clone();
            let key = key.clone();

            async move {
                tracing::info!("reloading web certificate");

                if let Err(err) = config.reload_from_pem_file(cert.as_ref(), key.as_ref()).await {
                    tracing::warn!(%err, "failed to reload web certificate");
                }
            }
                .boxed()
        })
            .await;

        if let Err(err) = result {
            tracing::warn!(%err, "failed to set up web certificate watcher, certificates will not be updated without a restart");
        }
    });
}