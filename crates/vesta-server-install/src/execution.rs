//! Internal direct-child ownership. Neither an arbitrary-command API nor a sandbox.
use super::{
    execution_workspace::Workspace, model::*, runtime::RuntimeBinding, transfer::controlled,
};
use crate::progress::ProgressReporter;
use std::{
    fmt,
    path::Path,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

pub(super) struct Budget {
    pub(super) deadline: Instant,
    cap: usize,
}
impl Budget {
    pub(super) fn new(duration: Duration, cap: usize) -> Result<Self> {
        if duration.is_zero()
            || duration > Duration::from_secs(600)
            || cap == 0
            || cap > 1024 * 1024
        {
            return Err(InstallError::InvalidInput);
        }
        Ok(Self {
            deadline: Instant::now() + duration,
            cap,
        })
    }
    #[cfg(test)]
    pub(super) fn expired() -> Self {
        Self {
            deadline: Instant::now() - Duration::from_secs(1),
            cap: 1024,
        }
    }
}

pub(super) struct Pool {
    slots: Arc<Semaphore>,
}
impl Pool {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(2)),
        })
    }
    pub(super) fn shared() -> Arc<Self> {
        static VALUE: OnceLock<Arc<Pool>> = OnceLock::new();
        VALUE.get_or_init(Self::new).clone()
    }
    #[cfg(test)]
    pub(super) fn available(&self) -> usize {
        self.slots.available_permits()
    }
    #[cfg(test)]
    pub(super) fn hold(&self) -> OwnedSemaphorePermit {
        self.slots.clone().try_acquire_owned().unwrap()
    }
    #[cfg(test)]
    pub(super) async fn hold_wait(&self) -> OwnedSemaphorePermit {
        tokio::time::timeout(Duration::from_secs(5), self.slots.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap()
    }
}

#[cfg(test)]
type SpawnObserver = Arc<dyn Fn(&Child) + Send + Sync>;

pub(super) struct Invocation {
    runtime: RuntimeBinding,
    args: Vec<String>,
    workspace: Option<Workspace>,
    pool: Arc<Pool>,
    #[cfg(test)]
    pub(super) faults: Faults,
    #[cfg(test)]
    pub(super) on_spawn: Option<SpawnObserver>,
    #[cfg(test)]
    pub(super) on_admission: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Invocation {
    pub(super) fn probe(path: &Path) -> Result<Self> {
        Self::bound(path, vec!["-version".into()], None)
    }
    fn bound(path: &Path, args: Vec<String>, workspace: Option<Workspace>) -> Result<Self> {
        let mut bytes = 0usize;
        if args.len() > 256 {
            return Err(InstallError::InvalidInput);
        }
        for arg in &args {
            bytes = bytes
                .checked_add(arg.len())
                .ok_or(InstallError::LimitExceeded)?;
            if arg.chars().count() > 4096 || arg.chars().any(char::is_control) || bytes > 64 * 1024
            {
                return Err(InstallError::InvalidInput);
            }
        }
        let runtime = RuntimeBinding::snapshot(path, 0)?;
        if workspace
            .as_ref()
            .is_some_and(|w| w.refuses_runtime(&runtime.path))
        {
            return Err(InstallError::PathRefused);
        }
        Ok(Self {
            runtime,
            args,
            workspace,
            pool: Pool::shared(),
            #[cfg(test)]
            faults: Faults::default(),
            #[cfg(test)]
            on_spawn: None,
            #[cfg(test)]
            on_admission: None,
        })
    }
    #[cfg(test)]
    pub(super) fn fixture(
        path: &Path,
        args: Vec<String>,
        workspace: Option<Workspace>,
        pool: Arc<Pool>,
    ) -> Result<Self> {
        let mut invocation = Self::bound(path, args, workspace)?;
        invocation.pool = pool;
        Ok(invocation)
    }
}
#[cfg(test)]
#[derive(Default)]
pub(super) struct Faults {
    pub(super) unconfirmed_reap: bool,
    pub(super) refuse_handoff: bool,
    pub(super) stall_reaper: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Disposition {
    NotStarted,
    Exited,
    Unconfirmed,
}
pub(super) struct Output {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) workspace: Option<Workspace>,
}
impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoundedChildOutput")
    }
}
#[derive(Debug)]
pub(super) struct Outcome {
    pub(super) result: Result<Output>,
    pub(super) disposition: Disposition,
}

pub(super) fn check(deadline: Instant, reporter: &dyn ProgressReporter) -> Result<()> {
    if reporter.is_cancelled() {
        return Err(InstallError::Cancelled);
    }
    if reporter.is_dry_run() {
        return Err(InstallError::RuntimeUnknown);
    }
    if Instant::now() >= deadline {
        return Err(InstallError::Deadline);
    }
    Ok(())
}

pub(super) async fn ready(deadline: Instant, reporter: &dyn ProgressReporter) -> Result<()> {
    loop {
        check(deadline, reporter)?;
        if !reporter.is_paused() {
            return Ok(());
        }
        tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(25))).await;
    }
}

pub(super) async fn read_output(mut pipe: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; 4096];
    loop {
        let n = pipe
            .read(&mut buffer)
            .await
            .map_err(|_| InstallError::RuntimeFailed)?;
        if n == 0 {
            return Ok(out);
        }
        if out.len().checked_add(n).is_none_or(|n| n > cap) {
            return Err(InstallError::LimitExceeded);
        }
        out.extend_from_slice(&buffer[..n]);
    }
}

struct Owner {
    child: Option<Child>,
    permit: Option<OwnedSemaphorePermit>,
    workspace: Option<Workspace>,
    confirmed: bool,
    refuse_handoff: bool,
    #[cfg(test)]
    stall_reaper: bool,
}
struct Pending {
    child: Child,
    permit: Option<OwnedSemaphorePermit>,
    workspace: Option<Workspace>,
    confirmed: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.confirmed {
            // A kill request/wait error is not exit proof. Never free unknown-effect protection.
            if let Some(permit) = self.permit.take() {
                permit.forget();
            }
            if let Some(workspace) = &mut self.workspace {
                workspace.quarantine();
            }
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let pending = Pending {
                child,
                permit: self.permit.take(),
                workspace: self.workspace.take(),
                confirmed: false,
            };
            if !self.refuse_handoff {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    #[cfg(test)]
                    let stall_reaper = self.stall_reaper;
                    handle.spawn(async move {
                        let mut pending = pending;
                        pending.confirmed = matches!(
                            tokio::time::timeout(Duration::from_secs(2), async {
                                #[cfg(test)]
                                if stall_reaper {
                                    std::future::pending::<()>().await;
                                }
                                pending.child.wait().await
                            })
                            .await,
                            Ok(Ok(_))
                        );
                    });
                    return;
                }
            }
            // Also runs if a handed-off task is dropped during runtime shutdown.
            drop(pending);
        }
    }
}

pub(super) async fn run(
    mut invocation: Invocation,
    budget: Budget,
    reporter: &dyn ProgressReporter,
) -> Outcome {
    let prepared = async {
        loop {
            ready(budget.deadline, reporter).await?;
            invocation.runtime.recheck()?;
            if let Some(workspace) = &invocation.workspace {
                workspace.verify_inputs(budget.deadline, reporter).await?;
            }
            #[cfg(test)]
            if let Some(hook) = &invocation.on_admission {
                hook();
            }
            let permit = controlled(
                async {
                    invocation
                        .pool
                        .slots
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|_| InstallError::Busy)
                },
                budget.deadline,
                reporter,
            )
            .await?;
            if reporter.is_paused() {
                drop(permit);
                continue;
            }
            invocation.runtime.recheck()?;
            if let Some(workspace) = &invocation.workspace {
                workspace.verify_inputs(budget.deadline, reporter).await?;
            }
            check(budget.deadline, reporter)?;
            if reporter.is_paused() {
                drop(permit);
                continue;
            }
            break Ok(permit);
        }
    }
    .await;
    let permit = match prepared {
        Ok(permit) => permit,
        Err(error) => {
            return Outcome {
                result: Err(error),
                disposition: Disposition::NotStarted,
            }
        }
    };
    let mut command = Command::new(&invocation.runtime.path);
    command
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in [
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "OPENJ9_JAVA_OPTIONS",
        "IBM_JAVA_OPTIONS",
    ] {
        command.env_remove(name);
    }
    if let Some(workspace) = &mut invocation.workspace {
        command.current_dir(workspace.cwd());
        workspace.started();
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            if let Some(workspace) = &mut invocation.workspace {
                workspace.spawn_failed();
            }
            return Outcome {
                result: Err(InstallError::RuntimeFailed),
                disposition: Disposition::NotStarted,
            };
        }
    };
    let mut owner = Owner {
        child: Some(child),
        permit: Some(permit),
        workspace: invocation.workspace.take(),
        confirmed: false,
        #[cfg(test)]
        refuse_handoff: invocation.faults.refuse_handoff,
        #[cfg(test)]
        stall_reaper: invocation.faults.stall_reaper,
        #[cfg(not(test))]
        refuse_handoff: false,
    };
    #[cfg(test)]
    if let Some(hook) = &invocation.on_spawn {
        hook(owner.child.as_ref().expect("owned spawned fixture"));
    }
    let result = controlled(
        async {
            let child = owner.child.as_mut().ok_or(InstallError::RuntimeFailed)?;
            let stdout = child.stdout.take().ok_or(InstallError::RuntimeFailed)?;
            let stderr = child.stderr.take().ok_or(InstallError::RuntimeFailed)?;
            let (stdout, stderr, status) = tokio::try_join!(
                read_output(stdout, budget.cap),
                read_output(stderr, budget.cap),
                async {
                    let status = child
                        .wait()
                        .await
                        .map_err(|_| InstallError::RuntimeFailed)?;
                    owner.confirmed = true;
                    Ok(status)
                }
            )?;
            if !status.success() {
                return Err(InstallError::RuntimeFailed);
            }
            invocation.runtime.recheck()?;
            if let Some(workspace) = &owner.workspace {
                workspace.verify_after(budget.deadline, reporter).await?;
            }
            Ok(Output {
                stdout,
                stderr,
                workspace: owner.workspace.take(),
            })
        },
        budget.deadline,
        reporter,
    )
    .await;
    if result.is_err() && !owner.confirmed {
        if let Some(child) = &mut owner.child {
            let _ = child.start_kill();
            #[cfg(test)]
            let attempt = !invocation.faults.unconfirmed_reap;
            #[cfg(not(test))]
            let attempt = true;
            if attempt {
                owner.confirmed = matches!(
                    tokio::time::timeout(Duration::from_secs(2), child.wait()).await,
                    Ok(Ok(_))
                );
            }
        }
    }
    Outcome {
        result,
        disposition: if owner.confirmed {
            Disposition::Exited
        } else {
            Disposition::Unconfirmed
        },
    }
}
