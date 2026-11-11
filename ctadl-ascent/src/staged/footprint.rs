//! A worker's footprint, reported to the scheduler over its stdout.

use std::io::Write;
use std::time::Duration;

use crate::index_engine::phys_footprint_mb;

/// How often a worker samples its footprint.
pub const PERIOD: Duration = Duration::from_millis(100);

/// What starts a footprint line. The rest is the footprint in MB.
const PREFIX: &str = "footprint_mb ";

/// Starts a thread that writes this process's footprint to stdout every [`PERIOD`], one line per
/// sample, until stdout closes or the process exits. Nothing else may write to stdout.
pub fn report_to_stdout() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("footprint".to_string())
        .spawn(|| {
            loop {
                let mb = phys_footprint_mb();
                let mut out = std::io::stdout().lock();
                if writeln!(out, "{PREFIX}{mb:.1}")
                    .and_then(|()| out.flush())
                    .is_err()
                {
                    break;
                }
                drop(out);
                std::thread::sleep(PERIOD);
            }
        })
        .map(|_| ())
}

/// The footprint in MB of a line [`report_to_stdout`] wrote, or `None` for any other line.
pub fn parse(line: &str) -> Option<f64> {
    line.strip_prefix(PREFIX)?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_line_parses() {
        assert_eq!(parse("footprint_mb 123.4"), Some(123.4));
        assert_eq!(parse("footprint_mb -1.0"), Some(-1.0));
        assert_eq!(parse("123.4"), None);
        assert_eq!(parse("footprint_mb x"), None);
    }
}
