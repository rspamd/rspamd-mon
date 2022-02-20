use clap::Parser;

use color_eyre::eyre::eyre;
use colored::Colorize;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use futures::lock::Mutex;
use log::LevelFilter;
use rasciigraph::{plot, Config};
use std::{
	collections::VecDeque,
	error::Error,
	io::{stdout, Write},
	sync::Arc,
	time::Duration,
};

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

struct CounterData<T> {
	/// Current counter value
	cur_value: T,
	/// Label for a counter
	label: &'static str,
}

/// A trait used to represent counters update
pub trait Counter<T> {
	/// Update for new value
	fn update(&mut self, new_value: T, ms: usize) -> Result<T, Box<dyn Error + Send + Sync>>;
	/// Creates a new counter
	fn new(label: &'static str) -> Self
	where
		Self: Sized + Send + Sync;
	fn label(&self) -> &'static str;
}

/// A counter which is used to represent gauge
struct GaugeCounter(CounterData<f64>);

impl Counter<f64> for GaugeCounter {
	fn update(&mut self, new_value: f64, _ms: usize) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let old_value = self.0.cur_value;
		self.0.cur_value = new_value;
		Ok(old_value)
	}

	fn new(label: &'static str) -> Self {
		Self(CounterData { cur_value: 0_f64, label })
	}

	fn label(&self) -> &'static str {
		self.0.label
	}
}

/// A counter that checks the difference
struct DiffCounter(CounterData<f64>);

impl Counter<f64> for DiffCounter {
	fn update(&mut self, new_value: f64, ms: usize) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let old_value = self.0.cur_value;
		let diff = if old_value.is_nan() { 0_f64 } else { new_value - old_value };
		self.0.cur_value = new_value;
		match ms {
			0 => Err("division by zero".to_owned().into()),
			_ => Ok(diff / (ms as f64)),
		}
	}

	fn new(label: &'static str) -> Self {
		Self(CounterData { cur_value: f64::NAN, label })
	}

	fn label(&self) -> &'static str {
		self.0.label
	}
}

/// Actions we recognize
#[derive(Clone, Copy)]
enum RspamdAction {
	ActionHam,
	ActionSpam,
	ActionJunk,
	ActionTotal,
	ActionUnknown,
}

impl Into<&'static str> for RspamdAction {
	fn into(self) -> &'static str {
		match self {
			RspamdAction::ActionHam => "ham",
			RspamdAction::ActionSpam => "spam",
			RspamdAction::ActionJunk => "junk",
			RspamdAction::ActionTotal => "total",
			RspamdAction::ActionUnknown => "unknown",
		}
	}
}

impl From<&'static str> for RspamdAction {
	fn from(s: &'static str) -> Self {
		match s {
			"no action" => RspamdAction::ActionHam,
			"no_action" => RspamdAction::ActionHam,
			"total" => RspamdAction::ActionTotal,
			"add header" => RspamdAction::ActionJunk,
			"add_header" => RspamdAction::ActionJunk,
			"rewrite subject" => RspamdAction::ActionJunk,
			"rewrite_subject" => RspamdAction::ActionJunk,
			_ => RspamdAction::ActionUnknown,
		}
	}
}

/// Used to track each action
struct RspamdStatElement {
	values: VecDeque<f64>,
	counter: Box<dyn Counter<f64> + Send>,
	nelts: usize,
}

impl RspamdStatElement {
	/// Creates a new stat element
	pub fn new(nelts: usize, action: RspamdAction, is_gauge: bool) -> Self {
		let counter: Box<dyn Counter<f64> + Send> = if is_gauge {
			Box::new(GaugeCounter::new(action.clone().into()))
		} else {
			Box::new(DiffCounter::new(action.clone().into()))
		};

		Self { values: VecDeque::with_capacity(nelts), counter, nelts }
	}

	pub fn update(&mut self, value: f64, elapsed: Duration) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let ms = elapsed.as_millis() as usize;
		let nv = self.counter.update(value, ms)?;

		// Expire one
		if self.values.len() >= self.nelts {
			self.values.pop_front();
		}

		self.values.push_back(nv);

		Ok(nv)
	}

	pub fn nelts(&self) -> usize {
		self.nelts
	}
}

/// Structure that holds all elements
struct RspamdStat {
	spam_stats: RspamdStatElement,
	ham_stats: RspamdStatElement,
	junk_stats: RspamdStatElement,
	total: RspamdStatElement,
}

impl RspamdStat {
	pub fn new(nelts: usize) -> Self {
		Self {
			spam_stats: RspamdStatElement::new(nelts, RspamdAction::ActionSpam, false),
			ham_stats: RspamdStatElement::new(nelts, RspamdAction::ActionHam, false),
			junk_stats: RspamdStatElement::new(nelts, RspamdAction::ActionJunk, false),
			total: RspamdStatElement::new(nelts, RspamdAction::ActionTotal, false),
		}
	}

	pub fn update_from_json(
		&mut self,
		json: serde_json::Value,
		elapsed: Duration,
	) -> Result<(), Box<dyn Error + Send + Sync>> {
		let actions = json.get("actions").ok_or(eyre!("missing actions"))?;
		let spam_cnt = update_specific_from_json(&mut self.spam_stats, actions, ["reject"].as_slice(), elapsed)?;
		let ham_cnt = update_specific_from_json(&mut self.ham_stats, actions, ["no action"].as_slice(), elapsed)?;
		let junk_cnt = update_specific_from_json(
			&mut self.junk_stats,
			actions,
			["add header", "rewrite subject"].as_slice(),
			elapsed,
		)?;
		self.total.update((spam_cnt + ham_cnt + junk_cnt) as f64, elapsed)?;
		Ok(())
	}

	pub fn display_chart(&self, max_height: u16) {
		show_specific_counter(&self.spam_stats, 0_u16, max_height);
		show_specific_counter(&self.ham_stats, 1_u16, max_height);
		show_specific_counter(&self.junk_stats, 2_u16, max_height);
		show_specific_counter(&self.total, 3_u16, max_height);
	}
}

fn show_specific_counter(elt: &RspamdStatElement, row: u16, max_height: u16) {
	if elt.values.len() == 0 {
		panic!("tried to display graph for an empty values");
	}

	let _ = stdout().queue(cursor::MoveTo(0, row * (max_height + 3)));

	let scaled_values: VecDeque<f64> = elt.values.iter().map(|v| *v * 1000_f64).collect();
	let avg = scaled_values.iter().sum::<f64>() / scaled_values.len() as f64;
	let min = *scaled_values.iter().min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0_f64);
	let max = *scaled_values.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0_f64);
	let last = *scaled_values.back().unwrap_or(&0.0);
	let plot_config = Config::default()
		.with_height(max_height as u32)
		.with_width(elt.nelts() as u32)
		.with_caption(format!(
			"Messages per second [Action: {}] [LAST: {}] [AVG: {}] [MIN: {}] [MAX: {}]",
			format!("{}", elt.counter.label()).bold(),
			format!("{:.2}", last).bright_purple().underline(),
			format!("{:.2}", avg).white().bold(),
			format!("{:.2}", min).green().bold(),
			format!("{:.2}", max).red().bold(),
		));
	let _ = stdout().write(format!("{}", plot(scaled_values.into(), plot_config)).as_bytes());
}

fn update_specific_from_json(
	elt: &mut RspamdStatElement,
	actions_json: &serde_json::Value,
	field: &[&'static str],
	elapsed: Duration,
) -> Result<f64, Box<dyn Error + Send + Sync>> {
	let total = field.iter().fold(0_u64, |acc, field| {
		let extracted = actions_json.get(field);
		let extracted = extracted.map(|v| v.as_u64().unwrap_or(0_u64));
		acc + extracted.unwrap_or(0_u64)
	});
	elt.update(total as f64, elapsed)?;
	Ok(total as f64)
}

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

	let _ = stdout().queue(Clear(ClearType::All)).unwrap();
	let stats = Arc::new(Mutex::new(RspamdStat::new(opts.chart_width)));

	tokio::spawn(async move {
		let stats = stats.clone();
		let mut niter = 0;
		loop {
			let _ = stdout().flush();
			let timeout = Duration::from_secs_f32(opts.timeout);
			let client = reqwest::Client::builder().timeout(timeout).user_agent("rspamd-mon").build()?;
			let req = client.get(opts.url.as_str()).send();
			let resp = match req.await {
				Ok(o) => o.bytes(),
				Err(e) => {
					return Err(eyre!("cannot get send request to {}: {}", opts.url.as_str(), e));
				},
			};
			match resp.await {
				Ok(o) => {
					let json: serde_json::Value = serde_json::from_slice(&o)
						.map_err(|e| eyre!("malformed json from {}: {}", opts.url.as_str(), e))?;
					let mut stats_unlocked = stats.lock().await;
					stats_unlocked
						.update_from_json(json, timeout)
						.map_err(|e| eyre!("cannot get results from {}: {}", opts.url.as_str(), e))?;
					if niter > 0 {
						stats_unlocked.display_chart(opts.chart_height as u16);
					}
					niter = niter + 1;

					Ok(())
				},
				Err(e) => Err(eyre!("cannot get results from {}: {}", opts.url.as_str(), e)),
			}?;
			tokio::time::sleep(timeout).await;
		}
	})
	.await?
}