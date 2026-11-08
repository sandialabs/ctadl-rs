/*! The cores and memory that the imports under one container share.

An APK's native libraries and an app bundle's splits are independent imports, and each native
library is a Ghidra run: a JVM with a large fixed cost to start and to analyze, which on a small
library keeps only a core or two busy. Running them one at a time leaves most of the machine
idle; running all of them at once, each with Ghidra's own defaults, asks for every core and a
40 GiB heap per JVM. A [`Budget`] sits in between. It holds a number of cores and an amount of
memory, and every run asks it for a share before it starts ([`Budget::acquire`]) and gives the
share back when it is done.

# Cores

`ctadl import -j N` sets the core budget. Without it, the budget is the cores the OS reports,
capped at [`DEFAULT_MAX_CORES`]: past that, a Ghidra run gains little from more threads, and
the rest of the machine is left to whatever else runs on it.

A share is counted in threads Ghidra may use (its `-max-cpu` and the JVM's
`ActiveProcessorCount`), and in-process work counts one. The in-process work itself -- lowering
facts to IR, encoding -- runs on one rayon pool with as many threads as the budget has cores
([`Budget::install`]), so a parallel step inside it cannot take more cores than the budget,
however many imports run it at once.

# Memory

The memory budget is [`MEMORY_ENV`] when it is set (the JVM's `-Xmx` syntax: `48G`, `4096M`),
else half the machine's physical memory. A Ghidra run's share is its JVM heap cap plus
[`JVM_OVERHEAD_MIB`] for the rest of the JVM, and the heap cap is what the JVM is started
with, so the JVMs running at once cannot together grow past the budget. The heap a library
gets follows its size (see `apk_native`); a share bigger than the whole budget is still
granted, but only when nothing else is running, so one huge library behaves as it would on its
own.

Once Ghidra exits, a run keeps a smaller share, an estimate of what lowering its facts takes in
this process. The Dex half of an APK is counted too, at an estimate from its Dex size, and only
one is lowered at a time ([`Budget::dex_turn`]).

# Order

Shares are granted strictly in the order they were asked for. A caller that asks for its
biggest work first therefore starts it first, and a big request is not starved by a stream of
small ones that fit around it.
*/

use std::sync::{Condvar, Mutex, MutexGuard};

/// The core budget when `-j` does not set one: past this, a Ghidra run gains little from more
/// threads.
pub const DEFAULT_MAX_CORES: usize = 16;

/// The environment variable that sets the memory budget, in the JVM's `-Xmx` syntax: a number
/// of bytes with an optional `K`, `M`, `G` or `T` suffix (powers of 1024), such as `48G`.
pub const MEMORY_ENV: &str = "CTADL_IMPORT_MEMORY";

/// What a JVM holds beyond its heap: metaspace, code cache, thread stacks, Ghidra's native
/// decompiler processes. Counted on top of every Ghidra run's heap cap.
pub const JVM_OVERHEAD_MIB: u64 = 1024;

/// The stack of every thread an import runs on here. The front ends recurse over the programs
/// they lower, and ran on the main thread before they ran here, so their threads get at least
/// the main thread's stack (8 MiB on Linux and macOS) rather than the 2 MiB a spawned thread
/// gets by default.
const STACK_SIZE: usize = 16 << 20;

/// Cores and memory to share among concurrent imports. See the module docs.
pub struct Budget {
    cores: usize,
    memory_mib: u64,
    state: Mutex<State>,
    changed: Condvar,
    /// Held while a Dex half is lowered; see [`Budget::dex_turn`].
    dex: Mutex<()>,
    pool: rayon::ThreadPool,
}

#[derive(Debug, Default)]
struct State {
    used_cores: usize,
    used_mib: u64,
    /// Grants outstanding.
    running: usize,
    /// The ticket the next [`Budget::acquire`] call takes.
    next_ticket: u64,
    /// The ticket whose turn it is; every lower one has been granted.
    serving: u64,
}

/// What one run asks a [`Budget`] for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// Cores. At least one is granted, and no more than the budget has.
    pub cores: usize,
    /// Memory, in MiB.
    pub mib: u64,
}

/// A share of a [`Budget`], given back when dropped.
pub struct Grant<'a> {
    budget: &'a Budget,
    cores: usize,
    mib: u64,
}

impl Budget {
    /// The budget `ctadl import -j jobs` runs under: `jobs` cores, or the default when `jobs` is
    /// 0, and the memory [`MEMORY_ENV`] sets or half of physical memory.
    pub fn from_jobs(jobs: usize) -> Self {
        let cores = if jobs > 0 {
            jobs
        } else {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(DEFAULT_MAX_CORES)
        };
        let memory_mib = match std::env::var(MEMORY_ENV) {
            Ok(text) => match parse_size_mib(&text) {
                Some(mib) => mib,
                None => {
                    log::warn!(
                        "ignoring {MEMORY_ENV}={text:?}: expected a size such as 48G or 4096M"
                    );
                    default_memory_mib()
                }
            },
            Err(_) => default_memory_mib(),
        };
        Self::new(cores, memory_mib)
    }

    /// A budget of exactly `cores` cores and `memory_mib` MiB.
    pub fn new(cores: usize, memory_mib: u64) -> Self {
        let cores = cores.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cores)
            .thread_name(|i| format!("ctadl-import-{i}"))
            .stack_size(STACK_SIZE)
            .build()
            .expect("spawning the import thread pool");
        log::debug!("import budget: {cores} core(s), {memory_mib} MiB");
        Self {
            cores,
            memory_mib: memory_mib.max(1),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            dex: Mutex::new(()),
            pool,
        }
    }

    /// The number of cores in the budget.
    pub fn cores(&self) -> usize {
        self.cores
    }

    /// The memory in the budget, in MiB.
    pub fn memory_mib(&self) -> u64 {
        self.memory_mib
    }

    /// Waits for, and holds until dropped, the one turn there is to lower a Dex half.
    ///
    /// Lowering one APK's Dex already keeps every thread of the pool busy, and takes gigabytes
    /// for a large app, so two at once would only compete for the same cores while doubling the
    /// memory. The splits of a bundle take turns; their native halves are not held up.
    pub fn dex_turn(&self) -> MutexGuard<'_, ()> {
        self.dex.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` on the budget's rayon pool, so any rayon work inside it shares the budget's
    /// cores with every other import doing the same.
    pub fn install<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        self.pool.install(f)
    }

    /// Blocks until `request` fits beside the grants outstanding, and until every earlier
    /// request has been granted, then grants it.
    ///
    /// A request bigger than the whole budget is granted once nothing else is running, with no
    /// more cores than the budget has: a run that cannot fit at all still runs, alone.
    pub fn acquire(&self, request: Request) -> Grant<'_> {
        let mut state = self.lock();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        let cores = request.cores.clamp(1, self.cores);
        loop {
            let fits = state.used_cores + cores <= self.cores
                && state.used_mib + request.mib <= self.memory_mib;
            if state.serving == ticket && (fits || state.running == 0) {
                break;
            }
            state = self
                .changed
                .wait(state)
                .expect("import budget lock poisoned");
        }
        state.used_cores += cores;
        state.used_mib += request.mib;
        state.running += 1;
        state.serving += 1;
        drop(state);
        // The next ticket may fit too.
        self.changed.notify_all();
        Grant {
            budget: self,
            cores,
            mib: request.mib,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("import budget lock poisoned")
    }
}

impl Grant<'_> {
    /// The cores granted.
    pub fn cores(&self) -> usize {
        self.cores
    }

    /// The memory granted, in MiB.
    pub fn mib(&self) -> u64 {
        self.mib
    }

    /// Gives back all but `cores` cores and `mib` MiB, for a run that has moved on to a phase
    /// that needs less. Never grows the grant.
    pub fn shrink(&mut self, cores: usize, mib: u64) {
        let cores = cores.min(self.cores);
        let mib = mib.min(self.mib);
        let mut state = self.budget.lock();
        state.used_cores -= self.cores - cores;
        state.used_mib -= self.mib - mib;
        drop(state);
        self.cores = cores;
        self.mib = mib;
        self.budget.changed.notify_all();
    }
}

impl Drop for Grant<'_> {
    fn drop(&mut self) {
        let mut state = self.budget.lock();
        state.used_cores -= self.cores;
        state.used_mib -= self.mib;
        state.running -= 1;
        drop(state);
        self.budget.changed.notify_all();
    }
}

/// Half of physical memory, in MiB: room for every JVM at once while leaving the rest to this
/// process and whatever else runs on the machine. 8 GiB when the OS does not say.
fn default_memory_mib() -> u64 {
    physical_memory_mib().map_or(8 * 1024, |mib| mib / 2)
}

/// The machine's physical memory, in MiB.
fn physical_memory_mib() -> Option<u64> {
    // SAFETY: `sysconf` reads a system constant and has no preconditions.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let bytes = u64::try_from(pages)
        .ok()?
        .checked_mul(u64::try_from(page_size).ok()?)?;
    (bytes > 0).then_some(bytes >> 20)
}

/// Parses a size in the JVM's `-Xmx` syntax into MiB, rounding down; `None` for anything else
/// or for less than 1 MiB.
fn parse_size_mib(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, shift) = match text.chars().last()?.to_ascii_uppercase() {
        'K' => (&text[..text.len() - 1], 10),
        'M' => (&text[..text.len() - 1], 20),
        'G' => (&text[..text.len() - 1], 30),
        'T' => (&text[..text.len() - 1], 40),
        _ => (text, 0),
    };
    let bytes = digits.parse::<u64>().ok()?.checked_mul(1u64 << shift)?;
    let mib = bytes >> 20;
    (mib > 0).then_some(mib)
}

/// Spawns `f` on a scoped thread with the stack every import thread gets here.
pub fn spawn_scoped<'scope, 'env, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    f: impl FnOnce() -> T + Send + 'scope,
) -> std::thread::ScopedJoinHandle<'scope, T> {
    std::thread::Builder::new()
        .name("ctadl-import-worker".to_string())
        .stack_size(STACK_SIZE)
        .spawn_scoped(scope, f)
        .expect("spawning an import worker thread")
}

/// Runs `work` over every item of `items` on up to `workers` threads, starting them in
/// `start_order` (indices into `items`), and hands each result to `report` in *index* order, as
/// soon as it and every result before it are in.
///
/// So whatever order the work finishes in, `report` -- and every log line it writes -- sees the
/// same sequence a serial loop would. Once a result satisfies `stop`, no item that has not
/// started yet is started, and `report` is called for nothing after that result; items already
/// running are waited for. With `start_order` in index order, every item before a stopping one
/// has started by the time it stops, so the result `report` stops at is the lowest-indexed one
/// that satisfies `stop`, as in a serial loop, not merely the first to finish.
pub fn run_ordered<T: Sync, R: Send>(
    items: &[T],
    start_order: &[usize],
    workers: usize,
    work: impl Fn(&T) -> R + Sync,
    stop: impl Fn(&R) -> bool + Sync,
    mut report: impl FnMut(usize, R),
) {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    debug_assert_eq!(start_order.len(), items.len());
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, R)>();
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, items.len().max(1)) {
            let tx = tx.clone();
            let (next, stopped, work, stop) = (&next, &stopped, &work, &stop);
            spawn_scoped(scope, move || {
                while !stopped.load(Ordering::Acquire) {
                    let Some(&index) = start_order.get(next.fetch_add(1, Ordering::AcqRel)) else {
                        break;
                    };
                    let result = work(&items[index]);
                    if stop(&result) {
                        stopped.store(true, Ordering::Release);
                    }
                    if tx.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut pending = BTreeMap::new();
        let mut due = 0;
        let mut reporting = true;
        for (index, result) in rx {
            pending.insert(index, result);
            while reporting && let Some(result) = pending.remove(&due) {
                due += 1;
                if stop(&result) {
                    reporting = false;
                }
                report(due - 1, result);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn sizes_parse_like_xmx() {
        assert_eq!(parse_size_mib("48G"), Some(48 * 1024));
        assert_eq!(parse_size_mib("4096m"), Some(4096));
        assert_eq!(parse_size_mib("1T"), Some(1024 * 1024));
        assert_eq!(parse_size_mib("2097152k"), Some(2048));
        assert_eq!(parse_size_mib("1073741824"), Some(1024));
        assert_eq!(parse_size_mib("12K"), None);
        assert_eq!(parse_size_mib("lots"), None);
        assert_eq!(parse_size_mib(""), None);
    }

    #[test]
    fn physical_memory_is_known() {
        assert!(physical_memory_mib().is_some_and(|mib| mib > 0));
    }

    /// The runs started at once never hold more cores or memory than the budget has.
    #[test]
    fn concurrent_grants_stay_within_the_budget() {
        let budget = Budget::new(4, 10);
        let (cores, mib, peak_cores, peak_mib) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        std::thread::scope(|scope| {
            for i in 0..12 {
                let (budget, cores, mib, peak_cores, peak_mib) =
                    (&budget, &cores, &mib, &peak_cores, &peak_mib);
                scope.spawn(move || {
                    let grant = budget.acquire(Request {
                        cores: 1 + i % 2,
                        mib: 1 + (i % 3) as u64,
                    });
                    let c = cores.fetch_add(grant.cores(), Ordering::SeqCst) + grant.cores();
                    let m = mib.fetch_add(grant.mib() as usize, Ordering::SeqCst)
                        + grant.mib() as usize;
                    peak_cores.fetch_max(c, Ordering::SeqCst);
                    peak_mib.fetch_max(m, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    cores.fetch_sub(grant.cores(), Ordering::SeqCst);
                    mib.fetch_sub(grant.mib() as usize, Ordering::SeqCst);
                });
            }
        });
        assert!(peak_cores.load(Ordering::SeqCst) <= 4);
        assert!(peak_mib.load(Ordering::SeqCst) <= 10);
    }

    /// A request bigger than the budget still runs, alone.
    #[test]
    fn an_oversized_request_runs_alone() {
        let budget = Budget::new(2, 10);
        let grant = budget.acquire(Request { cores: 8, mib: 100 });
        assert_eq!(grant.cores(), 2);
        assert_eq!(grant.mib(), 100);
    }

    /// Every item runs, and is reported once, in index order, whatever order they start in.
    #[test]
    fn every_result_is_reported_in_index_order() {
        let items: Vec<u64> = (0..20).collect();
        let start: Vec<usize> = (0..20).rev().collect();
        let mut seen = Vec::new();
        run_ordered(
            &items,
            &start,
            6,
            |&i| {
                std::thread::sleep(Duration::from_millis(i % 7));
                i
            },
            |_| false,
            |index, result| seen.push((index, result)),
        );
        let expected: Vec<(usize, u64)> = (0..20).map(|i| (i as usize, i)).collect();
        assert_eq!(seen, expected);
    }

    /// Shrinking a grant makes room for the next request.
    #[test]
    fn a_shrunk_grant_makes_room() {
        let budget = Budget::new(8, 100);
        let mut first = budget.acquire(Request { cores: 6, mib: 90 });
        first.shrink(1, 10);
        let second = budget.acquire(Request { cores: 7, mib: 90 });
        assert_eq!((first.cores(), first.mib()), (1, 10));
        assert_eq!((second.cores(), second.mib()), (7, 90));
    }

    /// Results come back in index order whatever order the work finishes in -- here, the later
    /// items finish first -- and the stopping result reported is the lowest-indexed one.
    #[test]
    fn results_are_reported_in_index_order() {
        let items: Vec<u64> = (0..20).collect();
        let start: Vec<usize> = (0..20).collect();
        let mut seen = Vec::new();
        run_ordered(
            &items,
            &start,
            6,
            |&i| {
                std::thread::sleep(Duration::from_millis(20 - i));
                i
            },
            |&i| i == 7 || i == 15,
            |index, result| seen.push((index, result)),
        );
        let expected: Vec<(usize, u64)> = (0..=7).map(|i| (i as usize, i)).collect();
        assert_eq!(seen, expected);
    }
}
