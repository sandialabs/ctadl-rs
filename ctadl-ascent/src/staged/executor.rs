//! Where the jobs of a staged run run: on the calling thread ([`InProcess`]), or each in a
//! worker process of its own ([`Process`]).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorContext};
use crate::facts::IdMap;
use crate::index_engine::Parallelism;
use crate::index_engine::path_set::PathSetRef;
use crate::staged::batch::{run_batch, run_intent_stage, share_path_set};
use crate::staged::footprint;
use ctadl_index::staged::plan::BatchId;
use ctadl_index::staged::store::StageDir;

/// One unit of work of a staged run that reads its inputs from the stage directory and writes
/// its outputs and marker there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    /// A stage-1 batch, in `round` if it is a part of a split SCC.
    Batch { id: BatchId, round: Option<u32> },
    /// The intent rules, once the global frame is done.
    Intent,
}

impl Job {
    /// A name for the job that is also a file name.
    pub fn name(&self) -> String {
        match self {
            Job::Batch { id, round: None } => format!("batch-{id}"),
            Job::Batch {
                id,
                round: Some(round),
            } => format!("batch-{id}-round-{round}"),
            Job::Intent => "intent".to_string(),
        }
    }

    /// The `ctadl index-worker` arguments that name the job, after the stage directory.
    pub fn worker_args(&self) -> Vec<String> {
        match self {
            Job::Batch { id, round } => {
                let mut args = vec!["--batch".to_string(), id.to_string()];
                if let Some(round) = round {
                    args.extend(["--round".to_string(), round.to_string()]);
                }
                args
            }
            Job::Intent => vec!["--intent".to_string()],
        }
    }

    /// Runs the job on the calling thread, on the engine of `parallelism`.
    pub fn run(
        &self,
        stage: &StageDir,
        parallelism: Parallelism,
        id_map: Option<&IdMap>,
    ) -> Result<(), Error> {
        match *self {
            Job::Batch { id, round } => {
                run_batch(stage, id, round, parallelism, id_map).map(|_| ())
            }
            Job::Intent => run_intent_stage(stage),
        }
    }
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::Batch { id, round: None } => write!(f, "batch {id}"),
            Job::Batch {
                id,
                round: Some(round),
            } => write!(f, "batch {id} in round {round}"),
            Job::Intent => write!(f, "the intent stage"),
        }
    }
}

/// Runs the jobs of a staged run, several at once if it can.
pub trait Executor {
    /// The threads it runs jobs on at once. A job takes one, or all of them when it runs alone.
    fn threads(&self) -> usize;

    /// Offers the jobs the path set of the run in `stage`, so they need not read it. An executor
    /// whose jobs run in other processes drops it.
    fn lend_path_set(&mut self, _stage: &StageDir, _path_set: PathSetRef) -> Result<(), Error> {
        Ok(())
    }

    /// Starts `job`, whose dependencies are done. When `alone`, no other job runs beside it until
    /// it ends, and it may use every thread.
    fn start(&mut self, stage: &StageDir, job: Job, alone: bool) -> Result<(), Error>;

    /// Waits for a started job to end, and returns it with how it ended. Call it only while a
    /// started job has not been returned.
    fn wait(&mut self) -> (Job, Result<(), Error>);

    /// Stops every started job that has not been returned, and forgets it.
    fn stop_all(&mut self);
}

/// Runs each job on the calling thread when it starts, one at a time, on the engine of
/// `parallelism`.
pub struct InProcess<'a> {
    pub parallelism: Parallelism,
    /// Names functions in log lines.
    pub id_map: Option<&'a IdMap>,
    ended: VecDeque<(Job, Result<(), Error>)>,
}

impl<'a> InProcess<'a> {
    pub fn new(parallelism: Parallelism, id_map: Option<&'a IdMap>) -> Self {
        Self {
            parallelism,
            id_map,
            ended: VecDeque::new(),
        }
    }
}

impl Executor for InProcess<'_> {
    fn threads(&self) -> usize {
        1
    }

    fn lend_path_set(&mut self, stage: &StageDir, path_set: PathSetRef) -> Result<(), Error> {
        share_path_set(stage, path_set)
    }

    fn start(&mut self, stage: &StageDir, job: Job, _alone: bool) -> Result<(), Error> {
        let result = job.run(stage, self.parallelism, self.id_map);
        self.ended.push_back((job, result));
        Ok(())
    }

    fn wait(&mut self) -> (Job, Result<(), Error>) {
        self.ended
            .pop_front()
            .expect("waiting for a job, but none was started")
    }

    fn stop_all(&mut self) {
        self.ended.clear();
    }
}

/// Runs each job in a worker process of its own, `ctadl index-worker`, at most as many at once as
/// `parallelism` has threads. A worker running alone runs the engine of `parallelism`; others
/// run the serial engine.
///
/// A worker's stdout carries its footprint samples, and its stderr goes to a file in the stage
/// directory's logs, so that a verbose worker never blocks on a full pipe.
pub struct Process {
    exe: PathBuf,
    parallelism: Parallelism,
    env: Vec<(OsString, OsString)>,
    workers: Vec<Worker>,
    /// Signalled by every footprint sample, and when a worker closes its stdout.
    wake: (mpsc::Sender<()>, mpsc::Receiver<()>),
}

/// A running worker process.
struct Worker {
    job: Job,
    child: Child,
    log: PathBuf,
    footprint: Arc<Footprint>,
    started: Instant,
    reader: Option<JoinHandle<()>>,
}

/// A worker's footprint samples, in MB, as `f64` bits.
#[derive(Default)]
struct Footprint {
    latest: AtomicU64,
    peak: AtomicU64,
}

impl Footprint {
    fn record(&self, mb: f64) {
        self.latest.store(mb.to_bits(), Ordering::Relaxed);
        if mb > f64::from_bits(self.peak.load(Ordering::Relaxed)) {
            self.peak.store(mb.to_bits(), Ordering::Relaxed);
        }
    }

    fn peak(&self) -> f64 {
        f64::from_bits(self.peak.load(Ordering::Relaxed))
    }
}

/// The most of a failed worker's log that its error quotes.
const LOG_TAIL_BYTES: u64 = 16 << 10;

/// How long [`Process::wait`] sleeps between looks at its workers when none reports.
const POLL: Duration = Duration::from_millis(50);

impl Process {
    /// Runs workers from `exe`, a `ctadl` executable.
    pub fn new(exe: impl Into<PathBuf>, parallelism: Parallelism) -> Self {
        Self {
            exe: exe.into(),
            parallelism,
            env: Vec::new(),
            workers: Vec::new(),
            wake: mpsc::channel(),
        }
    }

    /// Sets `key` to `value` in every worker's environment.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Spawns the worker of `job`, with `threads` threads.
    fn spawn(&self, stage: &StageDir, job: Job, threads: usize) -> Result<Worker, Error> {
        let log = stage.logs_dir().join(format!("{}.log", job.name()));
        let stderr = File::create(&log).err_context(|| format!("creating {}", log.display()))?;
        let mut child = Command::new(&self.exe)
            .arg("index-worker")
            .arg(stage.root())
            .args(job.worker_args())
            .args(["--jobs", &threads.to_string()])
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .err_context(|| format!("starting {} for {job}", self.exe.display()))?;
        let stdout = child.stdout.take().expect("the worker's stdout is piped");
        let footprint = Arc::new(Footprint::default());
        let reader = {
            let footprint = footprint.clone();
            let wake = self.wake.0.clone();
            std::thread::Builder::new()
                .name(format!("{} stdout", job.name()))
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        match footprint::parse(&line) {
                            Some(mb) => footprint.record(mb),
                            None => log::debug!("{job}: {line}"),
                        }
                        let _ = wake.send(());
                    }
                    let _ = wake.send(());
                })
                .err_context(|| format!("starting the stdout reader of {job}"))?
        };
        log::debug!("{job}: started a worker on {threads} threads");
        Ok(Worker {
            job,
            child,
            log,
            footprint,
            started: Instant::now(),
            reader: Some(reader),
        })
    }

    /// How the worker that exited with `status` ended.
    fn ended(mut worker: Worker, status: ExitStatus) -> (Job, Result<(), Error>) {
        if let Some(reader) = worker.reader.take() {
            let _ = reader.join();
        }
        let job = worker.job;
        log::debug!(
            "{job}: worker exited ({status}) after {:.2} s, peak footprint {:.1} MB, log in {}",
            worker.started.elapsed().as_secs_f64(),
            worker.footprint.peak(),
            worker.log.display()
        );
        if status.success() {
            return (job, Ok(()));
        }
        let tail = log_tail(&worker.log).unwrap_or_else(|e| format!("(unreadable: {e})"));
        (
            job,
            Err(Error::Staged(format!(
                "the worker of {job} failed ({status}); its log, {}, ends:\n{tail}",
                worker.log.display()
            ))),
        )
    }
}

impl Executor for Process {
    fn threads(&self) -> usize {
        match self.parallelism {
            Parallelism::Serial => 1,
            Parallelism::Threads(n) => n.get(),
        }
    }

    fn start(&mut self, stage: &StageDir, job: Job, alone: bool) -> Result<(), Error> {
        let threads = if alone { self.threads() } else { 1 };
        let worker = self.spawn(stage, job, threads)?;
        self.workers.push(worker);
        Ok(())
    }

    fn wait(&mut self) -> (Job, Result<(), Error>) {
        assert!(
            !self.workers.is_empty(),
            "waiting for a job, but none was started"
        );
        loop {
            for i in 0..self.workers.len() {
                match self.workers[i].child.try_wait() {
                    Ok(Some(status)) => return Self::ended(self.workers.swap_remove(i), status),
                    Ok(None) => {}
                    Err(e) => {
                        let worker = self.workers.swap_remove(i);
                        return (
                            worker.job,
                            Err(Error::from(e)).err_context(|| {
                                format!("waiting for the worker of {}", worker.job)
                            }),
                        );
                    }
                }
            }
            let _ = self.wake.1.recv_timeout(POLL);
        }
    }

    fn stop_all(&mut self) {
        for mut worker in self.workers.drain(..) {
            let _ = worker.child.kill();
            let _ = worker.child.wait();
            if let Some(reader) = worker.reader.take() {
                let _ = reader.join();
            }
            log::debug!("{}: worker stopped", worker.job);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// The end of the log file at `path`, at most [`LOG_TAIL_BYTES`] of it, from a line start.
fn log_tail(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let from = len.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(match (from, text.find('\n')) {
        (1.., Some(nl)) => text[nl + 1..].to_string(),
        _ => text.into_owned(),
    })
}
