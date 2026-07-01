use anyhow::Context;
use nix::sys::resource::Usage;
use nix::sys::time::TimeValLike;
use std::path::Path;
use std::time::Duration;

/// Reads csv from path
///
/// # Errors
///
/// If the file can't be read
///
/// # Panics
///
/// If deserializing a row fails
pub fn read_csv<T>(path: &Path) -> anyhow::Result<impl Iterator<Item = T>>
where
    for<'de> T: serde::de::Deserialize<'de>,
{
    let rdr = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .has_headers(false)
        .double_quote(false)
        .quoting(false)
        .from_path(path)
        .with_context(|| format!("Failed reading csv: {}", path.display()))?;
    Ok(rdr.into_deserialize().map(|x| x.unwrap()))
}

/// Writes csv to path
///
/// # Errors
///
/// If the file can't be written
pub fn write_csv(path: &Path) -> anyhow::Result<csv::Writer<std::fs::File>> {
    csv::WriterBuilder::new()
        .delimiter(b'\t')
        .has_headers(false)
        .quote_style(csv::QuoteStyle::Never)
        .from_path(path)
        .with_context(|| format!("Failed writing csv: {}", path.display()))
}

// Taken from https://github.com/dbueno/rusage
pub fn print_resources(wall_time: Duration, ru: &Usage) {
    let user_time = Duration::from_micros(ru.user_time().num_microseconds().try_into().unwrap());
    let system_time =
        Duration::from_micros(ru.system_time().num_microseconds().try_into().unwrap());
    println!("Wall time (secs):        {:.3}", wall_time.as_secs_f32());
    println!(
        "CPU time (secs):         user={:.3}; system={:.3}",
        user_time.as_secs_f32(),
        system_time.as_secs_f32()
    );
    println!("Max resident set size:   {}", ru.max_rss());
    println!("Integral shared memory:  {}", ru.shared_integral());
    println!("Integral unshared data:  {}", ru.unshared_data_integral());
    println!("Integral unshared stack: {}", ru.unshared_stack_integral());
    println!("Page reclaims:           {}", ru.minor_page_faults());
    println!("Page faults:             {}", ru.major_page_faults());
    println!("Swaps:                   {}", ru.full_swaps());
    println!(
        "Block I/Os:              input={}; output={}",
        ru.block_reads(),
        ru.block_writes()
    );
    println!("Signals received:        {}", ru.signals());
    println!(
        "IPC messages:            sent={}; received={}",
        ru.ipc_sends(),
        ru.ipc_receives()
    );
    println!(
        "Context switches:        voluntary={}; involuntary={}",
        ru.voluntary_context_switches(),
        ru.involuntary_context_switches()
    );
}
