use std::sync::{Arc, Mutex};
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

type ReloadListener = dyn Fn() + Send + Sync;

pub struct ConfigReloader {
	listeners: Arc<Mutex<Vec<Arc<ReloadListener>>>>,
}

impl ConfigReloader {
	pub fn new() -> Self {
		Self {
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
		#[cfg(unix)]
		let mut sigusr1 = signal(SignalKind::user_defined1())?;

		#[cfg(unix)]
		loop {
			let reload = tokio::select! {
				_ = async {
					sigusr1.recv().await;
				} => true,
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

		#[cfg(not(unix))]
		Ok(())
	}
}
