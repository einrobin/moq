mod auth;
mod cluster;
mod config;
mod connection;
mod reload;
mod web;

pub use auth::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use reload::*;
pub use web::*;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::load()?;

	let reloader_arc = Arc::new(ConfigReloader::new());

	let addr = config.server.bind.unwrap_or("[::]:443".parse().unwrap());
	let tls_config = config.server.tls.clone();
	let mut server = config.server.init()?;
	let client = config.client.init()?;
	let auth = config.auth.init()?;
	let fingerprints = server.fingerprints().to_vec();

	reloader_arc.clone().start_background_task();

	if !tls_config.cert.is_empty() {
		let server_reloader = server.reloader();
		let tls_config = tls_config.clone();

		reloader_arc.clone().watch_changes(move || {
			let server_reloader = server_reloader.clone();
			let tls_config = tls_config.clone();
			if let Err(err) = server_reloader.reload(&tls_config) {
				tracing::warn!(%err, "failed to reload server certificate");
			}
		});
	}

	let cluster = Cluster::new(config.cluster, client);
	let cloned = cluster.clone();
	tokio::spawn(async move { cloned.run().await.expect("cluster failed") });

	// Create a web server too.
	let web = Web::new(
		WebState {
			auth: auth.clone(),
			cluster: cluster.clone(),
			fingerprints,
			conn_id: Default::default(),
		},
		config.web,
		reloader_arc.clone(),
	);

	tokio::spawn(async move {
		web.run().await.expect("failed to run web server");
	});

	tracing::info!(%addr, "listening");

	// Notify systemd that we're ready after all initialization is complete
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	let mut conn_id = 0;

	while let Some(request) = server.accept().await {
		let conn = Connection {
			id: conn_id,
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
		};

		conn_id += 1;
		tokio::spawn(async move {
			let err = conn.run().await;
			if let Err(err) = err {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	Ok(())
}
