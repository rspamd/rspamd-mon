use accurate::{sum::Sum2, traits::SumWithAccumulator};
use color_eyre::eyre::eyre;

use std::{collections::VecDeque, error::Error, time::Duration};

use crate::plot::*;

pub struct CounterData<T> {
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
pub struct GaugeCounter(CounterData<f64>);

impl Counter<f64> for GaugeCounter {
	fn update(&mut self, new_value: f64, _ms: usize) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let old_value = self.0.cur_value;
		self.0.cur_value = new_value;
		Ok(old_value)
	}

	fn new(label: &'static str) -> Self {
		Self(CounterData { cur_value: f64::NAN, label })
	}

	fn label(&self) -> &'static str {
		self.0.label
	}
}

/// A counter that checks the difference
pub struct DiffCounter(CounterData<f64>);

impl Counter<f64> for DiffCounter {
	fn update(&mut self, new_value: f64, ms: usize) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let old_value = self.0.cur_value;
		let diff = if old_value.is_nan() { f64::NAN } else { new_value - old_value };
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

/// Counters we support
#[derive(Clone, Copy)]
pub enum KnownCounter {
	Ham,
	Spam,
	Junk,
	Total,
	AvgTime,
	Unknown,
}

impl From<KnownCounter> for &'static str {
	fn from(a: KnownCounter) -> &'static str {
		match a {
			KnownCounter::Ham => "ham msg/sec",
			KnownCounter::Spam => "spam msg/sec",
			KnownCounter::Junk => "junk msg/sec",
			KnownCounter::Total => "total msg/sec",
			KnownCounter::AvgTime => "average_time sec",
			KnownCounter::Unknown => "unknown",
		}
	}
}

impl From<&'static str> for KnownCounter {
	fn from(s: &'static str) -> Self {
		match s {
			"no action" => KnownCounter::Ham,
			"no_action" => KnownCounter::Ham,
			"total" => KnownCounter::Total,
			"add header" => KnownCounter::Junk,
			"add_header" => KnownCounter::Junk,
			"rewrite subject" => KnownCounter::Junk,
			"rewrite_subject" => KnownCounter::Junk,
			_ => KnownCounter::Unknown,
		}
	}
}

/// Used to track each action
pub struct RspamdStatElement {
	pub values: VecDeque<f64>,
	pub counter: Box<dyn Counter<f64> + Send>,
	pub nelts: usize,
}

impl RspamdStatElement {
	/// Creates a new stat element
	pub fn new(nelts: usize, action: KnownCounter, is_gauge: bool) -> Self {
		let counter: Box<dyn Counter<f64> + Send> = if is_gauge {
			Box::new(GaugeCounter::new(action.into()))
		} else {
			Box::new(DiffCounter::new(action.into()))
		};

		Self { values: VecDeque::with_capacity(nelts), counter, nelts }
	}

	pub fn update(&mut self, value: f64, elapsed: Duration) -> Result<f64, Box<dyn Error + Send + Sync>> {
		let ms = elapsed.as_millis() as usize;
		let nv = self.counter.update(value, ms)?;

		if !nv.is_nan() {
			// Expire one
			if self.values.len() >= self.nelts {
				self.values.pop_front();
			}

			self.values.push_back(nv);
		}

		Ok(nv)
	}

	pub fn nelts(&self) -> usize {
		self.nelts
	}
}

/// Structure that holds all elements
pub struct RspamdStat {
	spam_stats: RspamdStatElement,
	ham_stats: RspamdStatElement,
	junk_stats: RspamdStatElement,
	total: RspamdStatElement,
	avg_time: RspamdStatElement,
}

impl RspamdStat {
	/// Create new stats object
	pub fn new(nelts: usize) -> Self {
		Self {
			spam_stats: RspamdStatElement::new(nelts, KnownCounter::Spam, false),
			ham_stats: RspamdStatElement::new(nelts, KnownCounter::Ham, false),
			junk_stats: RspamdStatElement::new(nelts, KnownCounter::Junk, false),
			total: RspamdStatElement::new(nelts, KnownCounter::Total, false),
			avg_time: RspamdStatElement::new(nelts, KnownCounter::AvgTime, true),
		}
	}

	/// Update stats from JSON received from Rspamd
	pub fn update_from_json(
		&mut self,
		json: serde_json::Value,
		elapsed: Duration,
	) -> Result<(), Box<dyn Error + Send + Sync>> {
		let actions = json.get("actions").ok_or(eyre!("missing actions"))?;
		let spam_cnt =
			update_specific_from_json(&mut self.spam_stats, actions, ["reject"].as_slice(), elapsed, 1000.0_f64)?;
		let ham_cnt =
			update_specific_from_json(&mut self.ham_stats, actions, ["no action"].as_slice(), elapsed, 1000.0_f64)?;
		let junk_cnt = update_specific_from_json(
			&mut self.junk_stats,
			actions,
			["add header", "rewrite subject"].as_slice(),
			elapsed,
			1000.0_f64,
		)?;
		self.total.update((spam_cnt + ham_cnt + junk_cnt) as f64, elapsed)?;

		if let Some(scan_times) = json.get("scan_times") {
			if scan_times.is_array() {
				let scan_times = scan_times.as_array().unwrap();

				let avg_times = scan_times
					.iter()
					.map(|json_num| json_num.as_f64().unwrap_or(f64::NAN))
					.filter(|num| !num.is_nan())
					.collect::<Vec<_>>();
				if !avg_times.is_empty() {
					let cnt = avg_times.len() as f64;
					let avg_time = avg_times.sum_with_accumulator::<Sum2<_>>() / cnt;
					self.avg_time.update(avg_time, elapsed)?;
				}
			}
		}

		Ok(())
	}

	/// Display CLI plot
	pub fn display_plot(&self, max_height: u16) {
		prepare_term();
		let mut next_graph_pos = 0_u16;
		next_graph_pos = show_specific_counter(&self.spam_stats, next_graph_pos, max_height);
		next_graph_pos = show_specific_counter(&self.ham_stats, next_graph_pos, max_height);
		next_graph_pos = show_specific_counter(&self.junk_stats, next_graph_pos, max_height);
		next_graph_pos = show_specific_counter(&self.total, next_graph_pos, max_height);
		show_specific_counter(&self.avg_time, next_graph_pos, max_height);
		finalise_term();
	}
}

/// Update specific counter from a JSON object, multiplying value by `mult`
fn update_specific_from_json(
	elt: &mut RspamdStatElement,
	actions_json: &serde_json::Value,
	field: &[&'static str],
	elapsed: Duration,
	mult: f64,
) -> Result<f64, Box<dyn Error + Send + Sync>> {
	let total = field.iter().fold(0_u64, |acc, field| {
		let extracted = actions_json.get(field);
		let extracted = extracted.map(|v| v.as_u64().unwrap_or(0_u64));
		acc + extracted.unwrap_or(0_u64)
	}) as f64
		* mult;
	elt.update(total, elapsed)?;
	Ok(total)
}

#[cfg(test)]
mod tests {
	use crate::counters::{KnownCounter, RspamdStat, RspamdStatElement};
	use std::time::Duration;

	#[test]
	fn diff_counter_test() {
		let mut ctr = RspamdStatElement::new(2, KnownCounter::Unknown, false);
		let elapsed = Duration::from_millis(1);
		assert!(ctr.update(1_f64, elapsed).unwrap().is_nan());
		assert!(ctr.values.is_empty());
		assert_eq!(ctr.update(2_f64, elapsed).unwrap(), 1_f64);
		assert_eq!(ctr.values[0], 1_f64);
		assert_eq!(ctr.update(2_f64, elapsed).unwrap(), 0_f64);
		assert_eq!(ctr.values[0], 1_f64);
		assert_eq!(ctr.values[1], 0_f64);
		assert_eq!(ctr.update(3_f64, elapsed).unwrap(), 1_f64);
		assert_eq!(ctr.values[0], 0_f64);
		assert_eq!(ctr.values[1], 1_f64);
	}

	#[test]
	fn gauge_counter_test() {
		let mut ctr = RspamdStatElement::new(2, KnownCounter::Unknown, true);
		let elapsed = Duration::from_millis(1);
		assert!(ctr.update(1_f64, elapsed).unwrap().is_nan());
		assert!(ctr.values.is_empty());
		assert_eq!(ctr.update(2_f64, elapsed).unwrap(), 1_f64);
		assert_eq!(ctr.values[0], 1_f64);
		assert_eq!(ctr.update(2_f64, elapsed).unwrap(), 2_f64);
		assert_eq!(ctr.values[0], 1_f64);
		assert_eq!(ctr.values[1], 2_f64);
		assert_eq!(ctr.update(3_f64, elapsed).unwrap(), 2_f64);
		assert_eq!(ctr.values[0], 2_f64);
		assert_eq!(ctr.values[1], 2_f64);
	}

	#[test]
	fn update_from_json() {
		let json = r#"
		{"version":"3.2","config_id":"8nm93w87h5zfhzxxtfqy3k7sb5afrfx7u77fdg7d984pd53hair54rwdgcfk9yizc9kebg8x5f6r5bfz3jjz4gmcgxb4kf4iyhnxmbn","uptime":60901,"read_only":false,"scanned":3216735051,"learned":0,"actions":{"reject":995165214,"soft reject":0,"rewrite subject":0,"add header":4187423843,"greylist":275270625,"no action":2053842666},"scan_times":[0.507925,0.209795,0.223006,0.647264,0.529891,0.273673,0.537307,0.533161,0.539620,0.535399,0.540692,0.227740,0.540794,0.254937,0.498498,0.220530,0.477884,0.555480,0.502577,0.499710,0.424071,0.485661,0.505764,0.492892,0.495350,0.260113,0.597570,0.588293,0.501595,0.519670,0.504542],"spam_count":5182589057,"ham_count":2329113291,"connections":18424244,"control_connections":881,"pools_allocated":18425045,"pools_freed":18425058,"bytes_allocated":1884077939,"chunks_allocated":2597,"shared_chunks_allocated":15,"chunks_freed":0,"chunks_oversized":7268949,"fragmented":0,"total_learns":0,"statfiles":[]}
		"#;
		let elapsed = Duration::from_secs(1);
		let mut stats = RspamdStat::new(2);
		assert!(stats.update_from_json(serde_json::from_str(json).unwrap(), elapsed).is_ok());
	}
}
