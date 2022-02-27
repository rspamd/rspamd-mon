use clap::Parser;
use color_eyre::eyre::eyre;
use log::LevelFilter;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

use rspamd_mon::counters::RspamdStat;

#[derive(Debug, Parser)]
pub(crate) struct CliOpts {
	/// Chart width.
	#[clap(long, default_value = "80")]
	chart_width: usize,
	/// Chart height.
	#[clap(long, default_value = "6")]
	chart_height: usize,
	#[clap(name = "url", long, default_value = "http://localhost:11334/stat")]
	url: String,
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, parse(from_occurrences))]
	verbose: i8,
	/// How often do we poll Rspamd
	#[clap(long, default_value = "1.0")]
	timeout: f32,
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

	let stats = Arc::new(Mutex::new(RspamdStat::new(opts.chart_width)));

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
					if niter > 0 {
						stats_unlocked.display_plot(opts.chart_height as u16);
					}
					niter += 1;

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
				Ok(())
			}?;

			tokio::time::sleep(timeout).await;
		}
	})
	.await?
}
