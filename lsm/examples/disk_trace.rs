use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use lsm::{DiskLsmMultiMap, LsmDiskCodec};

type Key = (u64,);
type Value = (u64,);

const MEMTABLE_LIMIT: usize = 2;
const LEVEL_FANOUT: usize = 2;

fn main() -> io::Result<()> {
    let dir = trace_dir();
    let _ = fs::remove_dir_all(&dir);

    println!("trace directory: {}", dir.display());
    println!("memtable limit: {MEMTABLE_LIMIT} values");
    println!("level fanout: {LEVEL_FANOUT} runs");
    println!();

    let mut map =
        DiskLsmMultiMap::<Key, Value>::open_with_level_fanout(&dir, MEMTABLE_LIMIT, LEVEL_FANOUT)?;
    let writes = [
        ((3,), (30,)),
        ((1,), (10,)),
        ((2,), (20,)),
        ((1,), (11,)),
        ((4,), (40,)),
        ((2,), (21,)),
        ((5,), (50,)),
        ((3,), (31,)),
        ((6,), (60,)),
        ((1,), (12,)),
    ];

    for (step, (key, value)) in writes.into_iter().enumerate() {
        println!("step {}: insert key={key:?} value={value:?}", step + 1);
        map.insert(key, value);
        println!("logical len: {}", map.len());
        print_runs(&dir)?;
        println!();
    }

    println!("manual flush of any remaining memtable entries");
    map.flush()?;
    print_runs(&dir)?;
    println!();

    for key in [(1,), (2,), (404,)] {
        println!("get({key:?}) -> {:?}", map.get(&key)?);
    }
    println!();

    drop(map);
    println!("reopen from disk");
    let reopened =
        DiskLsmMultiMap::<Key, Value>::open_with_level_fanout(&dir, MEMTABLE_LIMIT, LEVEL_FANOUT)?;
    println!("reopened logical len: {}", reopened.len());
    println!("reopened get((1,)) -> {:?}", reopened.get(&(1,))?);
    println!();
    println!("leave files in place for inspection: {}", dir.display());

    Ok(())
}

fn trace_dir() -> PathBuf {
    std::env::temp_dir().join(format!("lsm-disk-trace-{}", std::process::id()))
}

fn print_runs(dir: &Path) -> io::Result<()> {
    let mut run_files = run_files(dir)?;
    if run_files.is_empty() {
        println!("runs on disk: none yet; writes are still buffered in the memtable");
        return Ok(());
    }

    println!("runs on disk:");
    for path in run_files.drain(..) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        println!("  {name}");
        for (key, values) in read_run(&path)? {
            println!("    key={key:?} values={values:?}");
        }
    }
    Ok(())
}

fn run_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("lsm") {
            files.push(path);
        }
    }
    files.sort_by_key(|path| path.file_name().map(|name| name.to_owned()));
    Ok(files)
}

fn read_run(path: &Path) -> io::Result<Vec<(Key, Vec<Value>)>> {
    let mut file = fs::File::open(path)?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != b"LSM1" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid run file magic",
        ));
    }

    let entry_count = read_u64(&mut file)?;
    let mut entries = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let key = Key::decode(&mut file)?;
        let value_count = read_u64(&mut file)?;
        let mut values = Vec::with_capacity(value_count as usize);
        for _ in 0..value_count {
            values.push(Value::decode(&mut file)?);
        }
        entries.push((key, values));
    }
    Ok(entries)
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
