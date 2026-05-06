use clap::{Parser, ValueHint};

/// Analyzes the convert result to find the call sites with the highest resolvents and print them
#[derive(Parser)]
struct Cli {
    /// The analysis project directory
    #[arg(value_hint = ValueHint::DirPath)]
    directory: String,
}

fn main() -> anyhow::Result<()> {
    ctadl_ascent::init();
    let _cli = Cli::parse();
    //let dir = std::path::Path::new(&cli.directory);
    //let config = ProjectConfig::find(dir)
    //    .with_context(|| format!("Failed to find analysis config: {}", dir.display()))?;
    //let result = ConvertResult::try_from(config.convert_facts())?;

    ////let mut counts = Vec::with_capacity(result.call.len());
    //let mut site_resolvents: Vec<_> = result
    //    .call
    //    .iter()
    //    .sorted_by_key(|(s, _)| s)
    //    .chunk_by(|(s, _)| s)
    //    .into_iter()
    //    .map(|(site, resolvent_iter)| (site, resolvent_iter.map(|(_, r)| r).collect::<Vec<_>>()))
    //    .collect();

    //site_resolvents.sort_by_key(|k| k.1.len());

    //let count = 50;
    //println!("Top {count} busiest call sites. Resolvent count and first resolvent");
    //let mut seen = HashSet::new();
    //for (_site, resolvents) in site_resolvents.iter().rev() {
    //    if seen.len() == count {
    //        break;
    //    }
    //    let num_resolvents = resolvents.len();
    //    for r in resolvents.iter().take(1) {
    //        if seen.insert(r) {
    //            println!("{}:{r}", num_resolvents);
    //        }
    //    }
    //}

    // let mut map = HashMap::new();
    // for n in counts {
    //     let count = map.entry(n).or_insert(0);
    //     *count += 1;
    // }
    // let values: Vec<_> = map.values().enumerate().collect();
    Ok(())
}
