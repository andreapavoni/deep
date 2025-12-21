//! Command runner abstraction for shelling out to system tools.

use anyhow::{Context, Result};
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Runner interface for invoking external commands.
pub trait Runner: Send + Sync {
    /// Execute a command and return its captured output.
    fn output(&self, program: &str, args: &[&str]) -> Result<Output>;
}

struct RealRunner;

impl Runner for RealRunner {
    fn output(&self, program: &str, args: &[&str]) -> Result<Output> {
        std::process::Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {} {:?}", program, args))
    }
}

static RUNNER: OnceLock<RwLock<Arc<dyn Runner>>> = OnceLock::new();
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn runner_lock() -> &'static RwLock<Arc<dyn Runner>> {
    RUNNER.get_or_init(|| RwLock::new(Arc::new(RealRunner)))
}

/// Run a command and capture its output.
pub fn run_output(program: &str, args: &[&str]) -> Result<Output> {
    let runner = runner_lock().read().expect("runner lock poisoned");
    runner.output(program, args)
}

/// Run a command and return its exit status.
pub fn run_status(program: &str, args: &[&str]) -> Result<ExitStatus> {
    Ok(run_output(program, args)?.status)
}

/// Check if a command is present on PATH.
pub fn command_exists(command: &str) -> bool {
    let probe = format!("command -v {}", command);
    run_status("sh", &["-c", &probe])
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Guard that restores the previous runner when dropped.
pub struct RunnerGuard {
    previous: Arc<dyn Runner>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for RunnerGuard {
    fn drop(&mut self) {
        let mut runner = runner_lock().write().expect("runner lock poisoned");
        *runner = self.previous.clone();
    }
}

/// Override the runner for tests; restores on guard drop.
pub fn set_runner_for_tests(runner: Arc<dyn Runner>) -> RunnerGuard {
    let lock = TEST_LOCK.get_or_init(|| Mutex::new(()));
    let guard = lock.lock().expect("runner test lock poisoned");
    let previous = {
        let mut slot = runner_lock().write().expect("runner lock poisoned");
        let previous = slot.clone();
        *slot = runner;
        previous
    };
    RunnerGuard {
        previous,
        _lock: guard,
    }
}
