use notify::{Config, EventKind, PollWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::Sleep;

pub struct ConfigReloader {
	paths: Vec<PathBuf>,
	listeners: Arc<Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>>,
}

impl ConfigReloader {
	pub fn new(paths: Vec<PathBuf>) -> Self {
		Self {
			paths,
			listeners: Arc::new(Mutex::new(Vec::new())),
		}
	}

	pub fn watch_changes<F>(&self, listener: F)
	where
		F: Fn() + Send + Sync + 'static,
	{
		self.listeners.lock().unwrap().push(Arc::new(listener));
	}

	pub fn start_background_task(self: Arc<Self>) {
		tokio::spawn(async move {
			let clone = self.clone();
			if let Err(err) = clone.start_watching().await {
				tracing::warn!(%err, "failed to watch for configuration changes");
			}
		});
	}

	pub async fn start_watching(&self) -> anyhow::Result<()> {
		let (tx, mut rx) = mpsc::channel(1);

		let config = Config::default()
			.with_poll_interval(Duration::from_secs(20))
			.with_follow_symlinks(true);

		// The polling method appears to be the only method that is actually reliable
		// and works 100 % with normal files, symlinks, symlinks to symlinks, ...
		let mut watcher = PollWatcher::new(
			move |res| {
				let _ = tx.blocking_send(res);
			},
			config,
		)?;

		for path in &self.paths {
			watcher.watch(path, RecursiveMode::NonRecursive)?;
		}

		#[cfg(unix)]
		let mut sigusr1 = signal(SignalKind::user_defined1())?;

		let mut debounce: Option<Pin<Box<Sleep>>> = None;

		loop {
			tokio::select! {
				_ = async {
					#[cfg(unix)]
					{
						sigusr1.recv().await;
					}
					#[cfg(not(unix))]
					{
						futures::future::pending::<()>().await;
					}
				} => {
					debounce = Some(Box::pin(tokio::time::sleep(Duration::from_secs(2))));
				},
				res = rx.recv() => {
					match res {
						Some(Ok(event)) => {
							if matches!(
								event.kind,
								EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
							) {
								debounce = Some(Box::pin(tokio::time::sleep(Duration::from_secs(2))));
							}
						}
						Some(Err(err)) => {
							tracing::warn!(%err, "watcher error");
						}
						None => break,
					}
				},
				_ = async {
					if let Some(timer) = debounce.as_mut() {
						timer.await;
						true
					} else {
						futures::future::pending::<bool>().await
					}
				} => {
					debounce = None;

					tracing::info!("reloading configuration");
					let listeners = {
						let lock = self.listeners.lock().unwrap();
						lock.clone()
					};

					for listener in listeners {
						listener();
					}
				}
			}
		}

		Ok(())
	}
}

pub fn build_watchable_paths(config: &crate::Config) -> Vec<PathBuf> {
	let mut paths = HashSet::new();

	paths.extend(config.server.tls.cert.iter().cloned());
	paths.extend(config.server.tls.key.iter().cloned());

	if let Some(cert) = &config.web.https.cert {
		paths.insert(cert.clone());
	}
	if let Some(key) = &config.web.https.key {
		paths.insert(key.clone());
	}

	paths.into_iter().collect()
}
