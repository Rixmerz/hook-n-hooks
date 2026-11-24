//! Generic fallback: ANSI already stripped by the runner; if the output is
//! longer than `generic_max_lines`, keep the first `head` and last `tail`
//! lines. Short outputs pass through unchanged.

use crate::config::Limits;
use crate::text::head_tail;

pub const NAME: &str = "generic";

pub fn filter(output: &str, limits: &Limits) -> String {
    head_tail(output, limits.generic_max_lines, limits.head, limits.tail)
        .unwrap_or_else(|| output.trim_end().to_string())
}
