use notify::{Config, EventKind, PollWatcher, RecursiveMode, Watcher};
use std::path::{PathBuf, Path};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

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
			.with_poll_interval(Duration::from_secs(5))
			.with_follow_symlinks(true);

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

		loop {
			let reload = tokio::select! {
				_ = async {
					#[cfg(unix)]
					{
						sigusr1.recv().await;
					}
					#[cfg(not(unix))]
					{
						futures::future::pending::<()>().await;
					}
				} => true,
				res = rx.recv() => {
					match res {
						Some(Ok(event)) => {
							matches!(
								event.kind,
								EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
							)
						}
						Some(Err(err)) => {
							tracing::warn!(%err, "watcher error");
							false
						}
						None => false,
					}
				}
			};

			if reload {
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
