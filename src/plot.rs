use crate::counters::RspamdStatElement;
use crossterm::{
	cursor,
	terminal::{Clear, ClearType},
	QueueableCommand,
};
use owo_colors::OwoColorize;
use rasciigraph::{plot, Config};
use std::io::{stdout, Write};

/// Draws a specific graph using CLI graphs
pub fn show_specific_counter(elt: &RspamdStatElement, row: u16, max_height: u16) -> u16 {
	if elt.values.is_empty() {
		return row;
	}

	let _ = stdout().queue(cursor::MoveTo(0, row * (max_height + 3)));
	let sliced_values: Vec<f64> = elt.values.iter().cloned().collect();
	let avg = sliced_values.iter().sum::<f64>() / sliced_values.len() as f64;
	let min = *sliced_values.iter().min_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0_f64);
	let max = *sliced_values.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap_or(&0_f64);
	let last = *sliced_values.last().unwrap_or(&0.0);
	let plot_config = Config::default()
		.with_height(max_height as u32)
		.with_width(elt.nelts() as u32)
		.with_caption(format!(
			"[Label: {}] [LAST: {}] [AVG: {}] [MIN: {}] [MAX: {}]",
			elt.counter.label().to_string().bold(),
			format!("{:.2}", last).bright_purple().underline(),
			format!("{:.2}", avg).white().bold(),
			format!("{:.2}", min).green().bold(),
			format!("{:.2}", max).red().bold(),
		));
	let _ = stdout().write(plot(sliced_values, plot_config).as_bytes());

	row + 1
}

/// Prepare terminal to show graphs
pub fn prepare_term() {
	let _ = stdout().queue(Clear(ClearType::All)).unwrap();
}

/// Flush graphs to stdout
pub fn finalise_term() {
	let _ = stdout().flush();
}
