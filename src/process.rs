use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub struct MediaLock {
    _file: File,
}

/// Linux must also reap media when the owning frontend is killed abruptly.
pub fn owned_child(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id();
        // Only async-signal-safe operations run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent as libc::pid_t {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    command
}
// Linux ties PDEATHSIG to the spawning thread. Spawn through a persistent
// supervisor so returning a child from a short-lived task does not kill it.
pub fn spawn_owned(command: Command) -> std::io::Result<Child> {
    type Request = (Command, std::sync::mpsc::SyncSender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<std::sync::mpsc::Sender<Request>> =
        std::sync::OnceLock::new();
    let sender = SPAWNER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<Request>();
        thread::Builder::new()
            .name("radio-process-spawner".into())
            .spawn(move || {
                for (mut command, reply) in receiver {
                    let child = owned_child(&mut command).spawn();
                    if let Err(error) = reply.send(child)
                        && let Ok(mut child) = error.0
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            })
            .expect("cannot start process supervisor");
        sender
    });
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    sender
        .send((command, reply))
        .map_err(|_| std::io::Error::other("process supervisor stopped"))?;
    result
        .recv()
        .map_err(|_| std::io::Error::other("process supervisor stopped"))?
}

impl MediaLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("media.lock"))?;
        file.try_lock_exclusive()
            .context("另一个 radio 会话正在播放或录音")?;
        Ok(Self { _file: file })
    }
}

pub fn drain(mut reader: impl Read + Send + 'static, cap: usize) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut result = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = cap.saturating_sub(result.len());
            result.extend_from_slice(&buf[..n.min(room)]);
        }
        result
    })
}

pub fn wait_until(child: &mut Child, timeout: Duration, stop: &AtomicBool) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("子进程已取消或超时");
        }
        thread::sleep(Duration::from_millis(30));
    }
}

pub fn output(command: &mut Command, timeout: Duration, stop: &Arc<AtomicBool>) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Move the configured command to the persistent spawning thread.
    let configured = std::mem::replace(command, Command::new("unused"));
    let mut child = spawn_owned(configured).context("无法启动依赖程序；请运行 radio doctor")?;
    let stdout = drain(child.stdout.take().unwrap(), 256 * 1024);
    let stderr = drain(child.stderr.take().unwrap(), 16 * 1024);
    let status = wait_until(&mut child, timeout, stop);
    let data = stdout.join().unwrap_or_default();
    let error = stderr.join().unwrap_or_default();
    if !status?.success() {
        bail!(
            "子进程失败: {}",
            crate::clean_text(&String::from_utf8_lossy(&error))
        );
    }
    Ok(data)
}

/// A cancellable task whose blocking I/O never holds up the media owner.
/// Dropping it cancels subprocesses and ignores any late network result.
pub struct Background<T> {
    cancel: Arc<AtomicBool>,
    result: std::sync::mpsc::Receiver<Result<T>>,
}
impl<T: Send + 'static> Background<T> {
    pub fn start(
        name: &str,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<T> + Send + 'static,
    ) -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (sender, result) = std::sync::mpsc::sync_channel(1);
        thread::Builder::new().name(name.into()).spawn(move || {
            let _ = sender.send(work(flag));
        })?;
        Ok(Self { cancel, result })
    }
    pub fn poll(&self) -> Option<Result<T>> {
        match self.result.try_recv() {
            Ok(value) => Some(value),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("后台任务异常退出")))
            }
        }
    }
}
impl<T> Drop for Background<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
