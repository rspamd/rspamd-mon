use clap::Parser;
use color_eyre::eyre::eyre;
use log::{info, LevelFilter};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[cfg(all(unix, feature = "drop_privs"))]
use privdrop::PrivDrop;

use rspamd_mon::counters::RspamdStat;

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct PlotOptions {
	/// Chart height.
	#[clap(long, default_value = "6")]
	chart_height: usize,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct PrometheusOptions {
	/// Prometheus endpoint port.
	#[clap(long, default_value = "65432")]
	port: u16,
}

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum CliMode {
	/// CLI chart mode.
	Plot(PlotOptions),
	/// Prometheus endpoint mode.
	Prometheus(PrometheusOptions),
}

#[derive(Debug, Parser)]
pub(crate) struct CliOpts {
	#[clap(name = "url", long, default_value = "http://localhost:11334/stat")]
	url: String,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, parse(from_occurrences))]
	verbose: i8,
	/// How often do we poll Rspamd
	#[clap(long, default_value = "1.0")]
	timeout: f32,
	/// Elements to store (and display)
	#[clap(long, default_value = "80")]
	num_elements: usize,
	#[clap(flatten)]
	#[cfg(all(unix, feature = "drop_privs"))]
	privdrop: PrivDropConfig,
	#[clap(subcommand)]
	mode: CliMode,
}

#[cfg(all(unix, feature = "drop_privs"))]
#[derive(Debug, Parser, Clone, Default)]
#[clap(rename_all = "kebab-case")]
struct PrivDropConfig {
	/// Run as this user and their primary group
	#[clap(short = 'u', long)]
	user: Option<String>,
	/// Run as this group
	#[clap(short = 'g', long)]
	group: Option<String>,
	/// Chroot to this directory
	#[clap(long)]
	chroot: Option<String>,
}

fn drop_privs(privdrop: &PrivDropConfig) {
	#[cfg(all(unix, feature = "drop_privs"))]
	let privdrop_enabled = [&privdrop.chroot, &privdrop.user, &privdrop.group].iter().any(|o| o.is_some());
	if privdrop_enabled {
		let mut pd = PrivDrop::default();
		if let Some(path) = &privdrop.chroot {
			info!("chroot: {}", path);
			pd = pd.chroot(path);
		}

		if let Some(user) = &privdrop.user {
			info!("setuid user: {}", user);
			pd = pd.user(user);
		}

		if let Some(group) = &privdrop.group {
			info!("setgid group: {}", group);
			pd = pd.group(group);
		}

		pd.apply().unwrap_or_else(|e| panic!("Failed to drop privileges: {}", e));

		info!("dropped privs");
	}
}

const MAX_NET_ERRORS: i32 = 5;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	color_eyre::install()?;

	let opts = CliOpts::parse();

	let log_level = match opts.verbose {
		0 => LevelFilter::Warn,
		1 => LevelFilter::Info,
		2 => LevelFilter::Debug,
		_ => LevelFilter::Trace,
	};

	env_logger::Builder::from_default_env()
		.filter(None, log_level)
		.format_timestamp(Some(env_logger::fmt::TimestampPrecision::Micros))
		.try_init()?;

	let stats = Arc::new(Mutex::new(RspamdStat::new(opts.num_elements)));
	drop_privs(&opts.privdrop);

	tokio::spawn(async move {
		let stats = stats.clone();
		let mut niter = 0;
		let mut error_counter = 0;
		let mut elapsed = Duration::from_secs_f32(opts.timeout);
		loop {
			let timeout = Duration::from_secs_f32(opts.timeout);
			let client = reqwest::Client::builder().timeout(timeout).user_agent("rspamd-mon").build()?;
			let req = client.get(opts.url.as_str()).send();
			let resp = match req.await {
				Ok(o) => o.bytes(),
				Err(e) => {
					// We should be able to send request
					return Err(eyre!("cannot get send request to {}: {}", opts.url.as_str(), e));
				},
			};
			let res = match resp.await {
				Ok(o) => {
					let json: serde_json::Value = serde_json::from_slice(&o)
						.map_err(|e| eyre!("malformed json from {}: {}", opts.url.as_str(), e))?;
					let mut stats_unlocked = stats.lock().await;
					stats_unlocked
						.update_from_json(json, elapsed)
						.map_err(|e| eyre!("cannot get results from {}: {}", opts.url.as_str(), e))?;

					Ok(())
				},
				Err(e) => Err(eyre!("cannot get results from {}: {}", opts.url.as_str(), e)),
			};

			let _ = if let Err(e) = res {
				error_counter += 1;
				elapsed = elapsed + elapsed;

				if error_counter > MAX_NET_ERRORS {
					Err(e)
				} else {
					Ok(())
				}
			} else {
				error_counter = 0;
				elapsed = timeout;

				if let CliMode::Plot(ref plot_opts) = opts.mode {
					let stats_unlocked = stats.lock().await;
					if niter > 0 {
						stats_unlocked.display_plot(plot_opts.chart_height as u16);
					} else {
						info!("connected to {}, waiting for data", opts.url.as_str());
					}
					niter += 1;
				}

				Ok(())
			}?;

			tokio::time::sleep(timeout).await;
		}
	})
	.await?
}
