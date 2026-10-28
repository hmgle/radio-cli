use crate::{
    config::Config,
    error::{DomainError, ErrorCode},
};
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Subcommand)]
pub enum Action {
    /// Install an isolated Python 3.12 speech runtime using uv.
    Setup,
    /// Check the runtime and selected model without downloading.
    Doctor,
    Models {
        #[command(subcommand)]
        action: ModelAction,
    },
}

#[derive(Subcommand)]
pub enum ModelAction {
    List,
    Install { id: String },
    Verify { id: String },
    Remove { id: String },
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Model {
    pub id: String,
    pub license: String,
    pub license_url: String,
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct ModelFile {
    pub path: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub(super) fn manifest() -> Vec<Model> {
    serde_json::from_str(include_str!("models.json")).expect("embedded speech manifest")
}

fn required_models(id: &str) -> Result<[Model; 2]> {
    required_from(manifest(), id)
}

pub(crate) fn validate_model(id: &str) -> Result<()> {
    required_models(id).map(|_| ())
}

fn required_from(models: Vec<Model>, id: &str) -> Result<[Model; 2]> {
    let mut selected = None;
    let mut vad = None;
    for model in models {
        if model.id == "silero-vad" {
            vad = Some(model);
        } else if model.id == id {
            selected = Some(model);
        }
    }
    Ok([
        selected.with_context(|| DomainError::new(ErrorCode::SubtitleModel).with("id", id))?,
        vad.context("embedded speech manifest has no silero-vad")?,
    ])
}

pub(super) fn root(config: &Config) -> PathBuf {
    config.data_dir.join("speech")
}
pub(super) fn runtime(config: &Config) -> PathBuf {
    root(config).join("runtime-2.0.1-v1")
}
pub(super) fn python(config: &Config) -> PathBuf {
    runtime(config).join("bin/python")
}

fn supports_runtime(os: &str, arch: &str, glibc: bool) -> bool {
    matches!((os, arch), ("macos", "x86_64" | "aarch64"))
        || (os == "linux" && arch == "x86_64" && glibc)
}

fn ensure_runtime_platform() -> Result<()> {
    // A statically linked musl radio binary can still use glibc Python on a
    // glibc host. Check the host loader rather than rejecting that binary.
    let glibc = cfg!(target_env = "gnu")
        || [
            "/lib64/ld-linux-x86-64.so.2",
            "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
        ]
        .iter()
        .any(|path| Path::new(path).exists());
    ensure!(
        supports_runtime(std::env::consts::OS, std::env::consts::ARCH, glibc),
        DomainError::new(ErrorCode::SubtitlePlatform).with(
            "platform",
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        )
    );
    Ok(())
}
pub(super) fn model_dir(config: &Config, id: &str) -> PathBuf {
    root(config).join("models").join(id)
}

pub(super) struct ResourceLock(File);
impl Drop for ResourceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(super) fn lock(config: &Config, exclusive: bool) -> Result<ResourceLock> {
    let path = root(config).join("resources.lock");
    let create = || -> Result<File> {
        fs::create_dir_all(root(config))?;
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?)
    };
    let file = if exclusive {
        create()
    } else {
        match File::open(&path) {
            Ok(file) => Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => create(),
            Err(error) => Err(error.into()),
        }
    }
    .with_context(|| format!("cannot open speech resource lock {}", path.display()))?;
    if exclusive {
        FileExt::try_lock_exclusive(&file)
    } else {
        FileExt::try_lock_shared(&file)
    }
    .context(DomainError::new(ErrorCode::SubtitleBusy))?;
    Ok(ResourceLock(file))
}

fn cancelled(stop: &AtomicBool) -> Result<()> {
    ensure!(
        !stop.load(Ordering::Relaxed),
        DomainError::new(ErrorCode::Cancelled)
    );
    Ok(())
}

fn digest(path: &Path, stop: &AtomicBool) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        cancelled(stop)?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn present(path: &Path, file: &ModelFile) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(s) => Ok(s.is_file() && s.len() == file.size),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("cannot inspect {}", path.display())),
    }
}

fn valid(path: &Path, file: &ModelFile, stop: &AtomicBool) -> Result<bool> {
    cancelled(stop)?;
    Ok(present(path, file)? && digest(path, stop)? == file.sha256)
}

#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    fn read(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(Self {
                device: meta.dev(),
                inode: meta.ino(),
                size: meta.len(),
                mode: meta.mode(),
                modified: (meta.mtime(), meta.mtime_nsec()),
                changed: (meta.ctime(), meta.ctime_nsec()),
            })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
        }
    }

    fn cacheable_at(&self, started: SystemTime) -> bool {
        // Filesystems can reuse a timestamp for successive writes. Do not
        // cache a hash until ctime is beyond that potentially racy second.
        started
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| i128::from(self.changed.0) + 1 < i128::from(now.as_secs()))
    }
}

/// Successful hashes are reusable only within this controller's lifetime.
/// Never trust persisted cache data or populate entries from metadata alone.
#[derive(Default)]
pub(super) struct VerificationCache {
    files: HashMap<PathBuf, (FileStamp, String)>,
    #[cfg(test)]
    hashes: usize,
}

impl VerificationCache {
    fn valid(&mut self, path: &Path, file: &ModelFile, stop: &AtomicBool) -> Result<bool> {
        self.valid_at(path, file, stop, SystemTime::now())
    }

    fn valid_at(
        &mut self,
        path: &Path,
        file: &ModelFile,
        stop: &AtomicBool,
        started: SystemTime,
    ) -> Result<bool> {
        cancelled(stop)?;
        let stamp = FileStamp::read(path)?;
        if let Some(stamp) = &stamp
            && stamp.size == file.size
            && self
                .files
                .get(path)
                .is_some_and(|(old, hash)| old == stamp && hash == &file.sha256)
        {
            return Ok(true);
        }
        self.files.remove(path);
        let Some(stamp) = stamp.filter(|stamp| stamp.size == file.size) else {
            return Ok(false);
        };
        #[cfg(test)]
        {
            self.hashes += 1;
        }
        if digest(path, stop)? != file.sha256 || FileStamp::read(path)?.as_ref() != Some(&stamp) {
            return Ok(false);
        }
        if stamp.cacheable_at(started) {
            self.files
                .insert(path.to_owned(), (stamp, file.sha256.clone()));
        }
        Ok(true)
    }

    pub(super) fn verify(&mut self, config: &Config, id: &str, stop: &AtomicBool) -> Result<()> {
        verify_with(config, id, |path, file| self.valid(path, file, stop))
    }
}

pub(super) fn verify(config: &Config, id: &str, stop: &AtomicBool) -> Result<()> {
    verify_with(config, id, |path, file| valid(path, file, stop))
}

fn verify_with(
    config: &Config,
    id: &str,
    mut valid_file: impl FnMut(&Path, &ModelFile) -> Result<bool>,
) -> Result<()> {
    for m in required_models(id)? {
        for file in &m.files {
            let path = model_dir(config, &m.id).join(&file.path);
            ensure!(
                valid_file(&path, file)?,
                DomainError::new(ErrorCode::SubtitleIntegrity)
                    .with("id", id)
                    .with("path", path.display())
            );
        }
    }
    Ok(())
}

/// A Range response is appended only when its complete range matches our file.
fn download(
    client: &reqwest::Client,
    id: &str,
    path: &Path,
    file: &ModelFile,
    stop: &AtomicBool,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _enter = runtime.enter();
    if valid(path, file, stop)? {
        return Ok(());
    }
    fs::create_dir_all(path.parent().unwrap())?;
    let partial = partial_path(path);
    let mut have = partial_len(&partial)?;
    if have == file.size && valid(&partial, file, stop)? {
        fs::rename(partial, path)?;
        return Ok(());
    }
    if have >= file.size {
        have = 0;
    }
    cancelled(stop)?;
    let mut request = client
        .get(&file.url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = runtime
        .block_on(network(request.send(), stop))??
        .error_for_status()?;
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let expected = format!("bytes {have}-{}/{}", file.size - 1, file.size);
        ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                == Some(expected.as_str()),
            "invalid Content-Range"
        );
    } else {
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "unexpected model download status"
        );
        have = 0;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(have == 0)
        .append(have > 0)
        .open(&partial)?;
    // A server may ignore Range, so recheck after any truncation as well as
    // checking the complete installation plan before the first request.
    check_space(
        file.size - have,
        fs2::available_space(path.parent().unwrap())?,
    )?;
    let mut last = Instant::now() - Duration::from_secs(2);
    loop {
        cancelled(stop)?;
        let Some(bytes) = runtime.block_on(network(response.chunk(), stop))?? else {
            break;
        };
        let n = bytes.len();
        ensure!(
            have + n as u64 <= file.size,
            "model download exceeds manifest size"
        );
        output.write_all(&bytes)?;
        have += n as u64;
        if last.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "{}",
                crate::t!("subtitle_install_progress", "file" => &file.path, "done" => have, "total" => file.size)
            );
            last = Instant::now();
        }
    }
    output.sync_all()?;
    drop(output);
    if have != file.size {
        bail!("model download truncated: {have}/{}", file.size);
    }
    if !valid(&partial, file, stop)? {
        return reject_corrupt_download(id, path);
    }
    fs::rename(partial, path)?;
    Ok(())
}

fn reject_corrupt_download(id: &str, path: &Path) -> Result<()> {
    let partial = partial_path(path);
    let integrity = DomainError::new(ErrorCode::SubtitleIntegrity)
        .with("id", id)
        .with("path", path.display());
    match fs::remove_file(&partial) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!(
                    "cannot remove corrupt download {}",
                    partial.display()
                ))
                .context(integrity));
        }
    }
    Err(integrity.into())
}

fn partial_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.part",
        path.extension().unwrap_or_default().to_string_lossy()
    ))
}

fn partial_len(path: &Path) -> Result<u64> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn sources(config: &Config, id: &str, file: &ModelFile) -> Result<Vec<String>> {
    let mut urls = Vec::new();
    if let Some(base) = &config.settings.subtitles.model_mirror {
        let mut url = reqwest::Url::parse(base)?;
        url.path_segments_mut()
            .map_err(|_| DomainError::new(ErrorCode::SubtitleSettings))?
            .pop_if_empty()
            .push(id)
            .extend(file.path.split('/'));
        urls.push(url.into());
    }
    urls.push(file.url.clone());
    Ok(urls)
}

fn install_file(
    config: &Config,
    client: &reqwest::Client,
    id: &str,
    file: &ModelFile,
    stop: &AtomicBool,
) -> Result<()> {
    let path = model_dir(config, id).join(&file.path);
    let mut last_error = None;
    let mut failures = Vec::new();
    for url in sources(config, id, file)? {
        let source = ModelFile {
            url,
            ..file.clone()
        };
        let resumed = partial_len(&partial_path(&path))? > 0;
        let mut result = download(client, id, &path, &source, stop);
        if resumed
            && source.url == file.url
            && result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<std::io::Error>().is_none())
            && result.as_ref().is_err_and(|e| {
                e.downcast_ref::<DomainError>()
                    .is_some_and(|e| e.code == ErrorCode::SubtitleIntegrity)
            })
        {
            // A failed mirror may have left a corrupt prefix. Only retry the
            // canonical source from scratch after a resumed digest mismatch;
            // a mirror mismatch should fall back to the canonical source.
            result = download(client, id, &path, &source, stop);
        }
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                cancelled(stop)?;
                failures.push(crate::redact::urls(&format!("{}: {error:#}", source.url)));
                // A second server cannot fix a local filesystem failure.
                if error.downcast_ref::<std::io::Error>().is_some()
                    || error
                        .downcast_ref::<DomainError>()
                        .is_some_and(|e| e.code == ErrorCode::SubtitleSpace)
                {
                    return Err(error.context(failures.join("; ")));
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error
        .expect("canonical source")
        .context(failures.join("; ")))
}

fn pending_bytes(config: &Config, models: &[Model], stop: &AtomicBool) -> Result<u64> {
    let mut pending = 0_u64;
    for model in models {
        for file in &model.files {
            let path = model_dir(config, &model.id).join(&file.path);
            if valid(&path, file, stop)? {
                continue;
            }
            let partial = partial_path(&path);
            let mut have = partial_len(&partial)?;
            if have >= file.size {
                if valid(&partial, file, stop)? {
                    continue;
                }
                have = 0;
            }
            pending = pending
                .checked_add(file.size - have)
                .context("model installation size overflow")?;
        }
    }
    Ok(pending)
}

fn check_space(pending: u64, available: u64) -> Result<()> {
    if pending > 0 {
        let required = pending.saturating_add(64 * 1024 * 1024);
        ensure!(
            available >= required,
            DomainError::new(ErrorCode::SubtitleSpace)
                .with("available", available)
                .with("required", required)
        );
    }
    Ok(())
}

fn model_list(config: &Config) -> Result<serde_json::Value> {
    let _lock = lock(config, false)?;
    let mut rows = Vec::new();
    for model in manifest().into_iter().filter(|m| m.id != "silero-vad") {
        let mut bytes = 0_u64;
        let mut all_present = true;
        for required in required_models(&model.id)? {
            for file in &required.files {
                bytes = bytes
                    .checked_add(file.size)
                    .context("model size overflow")?;
                all_present &= present(&model_dir(config, &required.id).join(&file.path), file)?;
            }
        }
        rows.push(serde_json::json!({"id":model.id,"bytes":bytes,"license":model.license,"license_url":model.license_url,"present":all_present}));
    }
    Ok(serde_json::json!(rows))
}

async fn network<T>(future: impl std::future::Future<Output = T>, stop: &AtomicBool) -> Result<T> {
    let timeout = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(future, timeout);
    loop {
        cancelled(stop)?;
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = &mut timeout => bail!("model download stalled"),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

pub(super) fn runtime_check(config: &Config, stop: &std::sync::Arc<AtomicBool>) -> Result<()> {
    cancelled(stop)?;
    ensure_runtime_platform()?;
    let result = crate::process::output(Command::new(python(config)).args(["-I", "-c", "import sherpa_onnx,numpy; assert sherpa_onnx.__version__ == '2.0.1'; assert numpy.__version__ == '2.2.6'"]), Duration::from_secs(15), stop);
    cancelled(stop)?;
    result.context(DomainError::new(ErrorCode::SubtitleRuntime))?;
    Ok(())
}

pub fn manage(
    config: &Config,
    action: Action,
    stop: &std::sync::Arc<AtomicBool>,
) -> Result<serde_json::Value> {
    use serde_json::json;
    match action {
        Action::Setup => {
            cancelled(stop)?;
            ensure_runtime_platform()?;
            let _lock = lock(config, true)?;
            if runtime_check(config, stop).is_err() {
                cancelled(stop)?;
                let requirements = root(config).join("requirements-2.0.1.lock");
                fs::write(&requirements, include_str!("requirements.lock"))?;
                let run = |command: &mut Command| -> Result<()> {
                    command.env_remove("UV_EXCLUDE_NEWER");
                    let bytes = crate::process::output(command, Duration::from_secs(600), stop)?;
                    eprintln!("{}", String::from_utf8_lossy(&bytes));
                    Ok(())
                };
                run(Command::new(&config.settings.subtitles.uv)
                    .args([
                        "--no-config",
                        "venv",
                        "--python",
                        "3.12",
                        "--managed-python",
                        "--clear",
                    ])
                    .arg(runtime(config)))?;
                run(Command::new(&config.settings.subtitles.uv)
                    .args([
                        "--no-config",
                        "pip",
                        "sync",
                        "--require-hashes",
                        "--only-binary",
                        ":all:",
                        "--default-index",
                        "https://pypi.org/simple/",
                        "--python",
                    ])
                    .arg(python(config))
                    .arg(requirements))?;
                runtime_check(config, stop)?;
            }
            Ok(json!({"runtime":runtime(config),"ready":true}))
        }
        Action::Doctor => {
            let _lock = lock(config, false)?;
            runtime_check(config, stop)?;
            verify(config, &config.settings.subtitles.model, stop)?;
            crate::process::output(
                Command::new(&config.settings.ffmpeg).arg("-version"),
                Duration::from_secs(5),
                stop,
            )?;
            Ok(
                json!({"ready":true,"runtime":runtime(config),"model":config.settings.subtitles.model}),
            )
        }
        Action::Models {
            action: ModelAction::List,
        } => model_list(config),
        Action::Models { action } => {
            if matches!(action, ModelAction::Install { .. }) {
                cancelled(stop)?;
                ensure_runtime_platform()?;
            }
            let id = match &action {
                ModelAction::Install { id }
                | ModelAction::Verify { id }
                | ModelAction::Remove { id } => id,
                ModelAction::List => unreachable!(),
            };
            let required = required_models(id)?;
            let _lock = lock(config, !matches!(action, ModelAction::Verify { .. }))?;
            match &action {
                ModelAction::Install { .. } => {
                    let pending = pending_bytes(config, &required, stop)?;
                    check_space(pending, fs2::available_space(root(config))?)?;
                    eprintln!(
                        "{}",
                        crate::t!("subtitle_install_plan", "id" => id, "bytes" => pending)
                    );
                    let client = reqwest::Client::builder()
                        .user_agent(crate::USER_AGENT)
                        .no_gzip()
                        .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
                        // network() bounds each wait and polls cancellation;
                        // a progressing large download has no total deadline.
                        .build()?;
                    for m in required {
                        for f in &m.files {
                            install_file(config, &client, &m.id, f, stop)?;
                        }
                    }
                    verify(config, id, stop)?;
                }
                ModelAction::Verify { .. } => verify(config, id, stop)?,
                ModelAction::Remove { .. } => {
                    let dir = model_dir(config, id);
                    if dir.exists() {
                        fs::remove_dir_all(dir)?;
                    }
                }
                ModelAction::List => unreachable!(),
            }
            Ok(json!({"id":id,"removed":matches!(action, ModelAction::Remove{..})}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Server, response};

    fn verification_fixture(directory: &Path) -> (PathBuf, ModelFile) {
        let file = ModelFile {
            path: "model.onnx".into(),
            url: "http://127.0.0.1:1".into(),
            size: 6,
            sha256: format!("{:x}", Sha256::digest(b"abcdef")),
        };
        let path = directory.join(&file.path);
        fs::write(&path, b"abcdef").unwrap();
        let stamp = FileStamp::read(&path).unwrap().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stamp.cacheable_at(SystemTime::now()) {
            assert!(Instant::now() < deadline, "file timestamp did not age");
            std::thread::sleep(Duration::from_millis(10));
        }
        (path, file)
    }

    #[test]
    fn verification_cache_does_not_trust_recent_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let (path, file) = verification_fixture(dir.path());
        // A fresh write must not be cached even when its digest is correct.
        fs::write(&path, b"abcdef").unwrap();
        let stamp = FileStamp::read(&path).unwrap().unwrap();
        let second = u64::try_from(stamp.changed.0).unwrap();
        for offset in [0, 1, 2] {
            assert_eq!(
                stamp.cacheable_at(UNIX_EPOCH + Duration::from_secs(second + offset)),
                offset == 2
            );
        }
        let mut cache = VerificationCache::default();
        assert!(
            cache
                .valid_at(
                    &path,
                    &file,
                    &AtomicBool::new(false),
                    UNIX_EPOCH + Duration::from_secs(second)
                )
                .unwrap()
        );
        assert!(cache.files.is_empty());
    }

    #[test]
    fn verification_cache_reuses_hashes_but_checks_manifest_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let (path, file) = verification_fixture(dir.path());
        let stop = AtomicBool::new(false);
        let mut cache = VerificationCache::default();
        assert!(cache.valid(&path, &file, &stop).unwrap());
        assert!(cache.valid(&path, &file, &stop).unwrap());
        assert_eq!(cache.hashes, 1);

        stop.store(true, Ordering::Relaxed);
        let error = cache.valid(&path, &file, &stop).unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::Cancelled
        );
        stop.store(false, Ordering::Relaxed);
        let other = ModelFile {
            sha256: format!("{:x}", Sha256::digest(b"xxxxxx")),
            ..file.clone()
        };
        assert!(!cache.valid(&path, &other, &stop).unwrap());
        assert!(cache.files.is_empty());
        assert_eq!(cache.hashes, 2);
        assert!(cache.valid(&path, &file, &stop).unwrap());
        assert_eq!(cache.hashes, 3);

        let mut fresh = VerificationCache::default();
        assert!(fresh.valid(&path, &file, &stop).unwrap());
        assert_eq!(fresh.hashes, 1);
    }

    #[test]
    fn verification_cache_rehashes_changed_files_even_with_restored_mtime() {
        use std::fs::FileTimes;
        for replacement in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (path, file) = verification_fixture(dir.path());
            let stop = AtomicBool::new(false);
            let mut cache = VerificationCache::default();
            assert!(cache.valid(&path, &file, &stop).unwrap());
            let original = fs::metadata(&path).unwrap();
            if replacement {
                let other = dir.path().join("replacement");
                fs::write(&other, b"xxxxxx").unwrap();
                fs::rename(other, &path).unwrap();
            } else {
                fs::write(&path, b"xxxxxx").unwrap();
            }
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(original.modified().unwrap()))
                .unwrap();
            let changed = fs::metadata(&path).unwrap();
            assert_eq!(changed.modified().unwrap(), original.modified().unwrap());
            assert_eq!(changed.len(), original.len());
            assert!(!cache.valid(&path, &file, &stop).unwrap());
            assert_eq!(cache.hashes, 2);
            assert!(cache.files.is_empty());
            // A failed verification must not become a reusable result.
            assert!(!cache.valid(&path, &file, &stop).unwrap());
            assert_eq!(cache.hashes, 3);
        }
    }

    #[test]
    fn verification_cache_rejects_missing_resized_and_nonregular_files() {
        use std::os::unix::fs::symlink;
        for mode in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let (path, file) = verification_fixture(dir.path());
            let stop = AtomicBool::new(false);
            let mut cache = VerificationCache::default();
            assert!(cache.valid(&path, &file, &stop).unwrap());
            fs::remove_file(&path).unwrap();
            match mode {
                0 => {}
                1 => fs::write(&path, b"short").unwrap(),
                2 => fs::create_dir(&path).unwrap(),
                _ => {
                    let target = dir.path().join("target");
                    fs::write(&target, b"abcdef").unwrap();
                    symlink(target, &path).unwrap();
                }
            }
            assert!(!cache.valid(&path, &file, &stop).unwrap());
            assert!(cache.files.is_empty());
            assert_eq!(cache.hashes, 1);
        }
    }

    #[test]
    fn verification_cache_preserves_permission_errors() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (path, file) = verification_fixture(dir.path());
        let stop = AtomicBool::new(false);
        let mut cache = VerificationCache::default();
        assert!(cache.valid(&path, &file, &stop).unwrap());
        let permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let cached = cache.valid(&path, &file, &stop);
        let full = valid(&path, &file, &stop);
        fs::set_permissions(&path, permissions).unwrap();
        for result in [cached, full] {
            assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
        assert!(cache.files.is_empty());
        assert!(cache.valid(&path, &file, &stop).unwrap());
    }

    #[test]
    fn shared_locks_open_existing_resources_without_write_access() {
        use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        drop(lock(&config, true).unwrap());
        let reader = lock(&config, false).unwrap();
        let flags = unsafe { libc::fcntl(reader.0.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        assert!(lock(&config, true).is_err());
        drop(reader);
        assert!(lock(&config, true).is_ok());

        let directory = root(&config);
        let path = directory.join("resources.lock");
        let dir_permissions = fs::metadata(&directory).unwrap().permissions();
        let file_permissions = fs::metadata(&path).unwrap().permissions();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let result = model_list(&config);
        fs::set_permissions(&directory, dir_permissions).unwrap();
        fs::set_permissions(&path, file_permissions).unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn read_only_directory_without_a_lock_reports_why_it_cannot_coordinate() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // Root bypasses the permission boundary this test exercises.
        }
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let directory = root(&config);
        fs::create_dir_all(&directory).unwrap();
        let permissions = fs::metadata(&directory).unwrap().permissions();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        let result = model_list(&config);
        fs::set_permissions(&directory, permissions).unwrap();
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(error.to_string().contains("resources.lock"));
        assert!(!directory.join("resources.lock").exists());
    }

    #[test]
    fn corrupt_download_cleanup_keeps_integrity_context_and_tolerates_missing_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.onnx");
        let error = reject_corrupt_download("test-model", &path).unwrap_err();
        let integrity = error.downcast_ref::<DomainError>().unwrap();
        assert_eq!(integrity.code, ErrorCode::SubtitleIntegrity);
        assert_eq!(integrity.params["id"], "test-model");
        assert_eq!(integrity.params["path"], path.to_string_lossy());
        assert!(error.downcast_ref::<std::io::Error>().is_none());

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let partial = partial_path(&path);
        fs::write(&partial, b"corrupt").unwrap();
        let permissions = fs::metadata(dir.path()).unwrap().permissions();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let result = reject_corrupt_download("test-model", &path);
        fs::set_permissions(dir.path(), permissions).unwrap();
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::SubtitleIntegrity
        );
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(format!("{error:#}").contains("cannot remove corrupt download"));
        assert!(partial.exists());
    }

    #[test]
    fn cleanup_permission_errors_keep_source_diagnostics_without_retrying() {
        use std::{
            os::unix::fs::PermissionsExt,
            sync::{Arc, atomic::AtomicUsize},
        };
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::test_support::config(dir.path());
        let path = model_dir(&config, "test-model").join("model.onnx");
        let directory = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&directory).unwrap();
        fs::write(partial_path(&path), b"xxx").unwrap();
        let permissions = fs::metadata(&directory).unwrap().permissions();
        let protected = directory.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let origin = Server::new(move |_, stream| {
            counter.fetch_add(1, Ordering::Relaxed);
            fs::set_permissions(&protected, fs::Permissions::from_mode(0o555)).unwrap();
            response(stream, 200, "application/octet-stream", b"xxxxxx");
        });
        let mirror = Server::new(|_, stream| response(stream, 404, "text/plain", b"missing"));
        config.settings.subtitles.model_mirror = Some(mirror.url.clone());
        let file = ModelFile {
            path: "model.onnx".into(),
            url: origin.url.clone(),
            size: 6,
            sha256: format!("{:x}", Sha256::digest(b"abcdef")),
        };
        let result = install_file(
            &config,
            &reqwest::Client::new(),
            "test-model",
            &file,
            &AtomicBool::new(false),
        );
        fs::set_permissions(&directory, permissions).unwrap();
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().params["id"],
            "test-model"
        );
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains(&mirror.url));
        assert!(diagnostic.contains(&origin.url));
        assert!(diagnostic.contains(&path.to_string_lossy().to_string()));
        assert!(diagnostic.contains("cannot remove corrupt download"));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn runtime_platform_support_checks_host_libc() {
        assert!(supports_runtime("linux", "x86_64", true));
        assert!(!supports_runtime("linux", "x86_64", false));
        assert!(!supports_runtime("linux", "aarch64", true));
        assert!(supports_runtime("macos", "x86_64", false));
        assert!(supports_runtime("macos", "aarch64", false));
        assert!(!supports_runtime("windows", "x86_64", false));
    }

    #[test]
    fn cancelled_runtime_checks_do_not_start_installation_or_mask_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = std::sync::Arc::new(AtomicBool::new(true));
        for error in [
            runtime_check(&config, &stop).unwrap_err(),
            manage(&config, Action::Setup, &stop).unwrap_err(),
            manage(
                &config,
                Action::Models {
                    action: ModelAction::Install {
                        id: "sensevoice-small-yue-int8".into(),
                    },
                },
                &stop,
            )
            .unwrap_err(),
        ] {
            assert_eq!(
                error.downcast_ref::<DomainError>().unwrap().code,
                ErrorCode::Cancelled
            );
        }
        assert!(!root(&config).exists());
    }

    #[test]
    fn model_install_checks_platform_before_model_lookup_or_disk_changes() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let expected = if ensure_runtime_platform().is_ok() {
            ErrorCode::SubtitleModel
        } else {
            ErrorCode::SubtitlePlatform
        };
        let error = manage(
            &config,
            Action::Models {
                action: ModelAction::Install {
                    id: "unknown-model".into(),
                },
            },
            &std::sync::Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
        assert_eq!(error.downcast_ref::<DomainError>().unwrap().code, expected);
        assert!(!root(&config).exists());
    }

    #[test]
    fn network_waits_remain_cancellable_and_abort_when_stalled() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let stop = AtomicBool::new(false);
            let cancel = async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                stop.store(true, Ordering::Relaxed);
            };
            let wait = network(std::future::pending::<()>(), &stop);
            let (result, ()) =
                tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(wait, cancel) })
                    .await
                    .expect("network cancellation must remain responsive");
            assert_eq!(
                result
                    .unwrap_err()
                    .downcast_ref::<DomainError>()
                    .unwrap()
                    .code,
                ErrorCode::Cancelled
            );

            stop.store(false, Ordering::Relaxed);
            let result = tokio::time::timeout(
                Duration::from_secs(25),
                network(std::future::pending::<()>(), &stop),
            )
            .await
            .expect("stalled network waits must time out");
            assert_eq!(result.unwrap_err().to_string(), "model download stalled");
        });
    }

    #[test]
    fn mirrors_preserve_base_paths_and_fall_back_without_changing_hashes() {
        use std::sync::{Arc, atomic::AtomicUsize};
        // Successful mirror, bad digest, missing file, corrupt prefix left by
        // a mirror, and resumed mirror mismatches with Range honored/ignored.
        for mode in 0..6 {
            let origin_calls = Arc::new(AtomicUsize::new(0));
            let calls = origin_calls.clone();
            let origin = Server::new(move |request, stream| {
                let attempt = calls.fetch_add(1, Ordering::Relaxed);
                if mode == 3 && attempt == 0 {
                    assert!(request.to_ascii_lowercase().contains("range: bytes=3-"));
                    stream.write_all(b"HTTP/1.1 206 OK\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef").unwrap();
                } else {
                    assert!(!request.to_ascii_lowercase().contains("range:"));
                    response(stream, 200, "application/octet-stream", b"abcdef");
                }
            });
            let mirror_calls = Arc::new(AtomicUsize::new(0));
            let calls = mirror_calls.clone();
            let mirror = Server::new(move |request, stream| {
                calls.fetch_add(1, Ordering::Relaxed);
                assert!(request.starts_with("GET /base/test-model/tokenizer/vocab.json "));
                match mode {
                    0 => response(stream, 200, "application/octet-stream", b"abcdef"),
                    1 | 5 => response(stream, 200, "application/octet-stream", b"xxxxxx"),
                    2 => response(stream, 404, "text/plain", b"missing"),
                    3 => response(stream, 200, "application/octet-stream", b"xxx"),
                    _ => {
                        assert!(request.to_ascii_lowercase().contains("range: bytes=3-"));
                        stream.write_all(b"HTTP/1.1 206 OK\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxxx").unwrap();
                    }
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let mut config = crate::test_support::config(dir.path());
            config.settings.subtitles.model_mirror = Some(format!("{}/base/", mirror.url));
            let file = ModelFile {
                path: "tokenizer/vocab.json".into(),
                url: origin.url.clone(),
                size: 6,
                sha256: format!("{:x}", Sha256::digest(b"abcdef")),
            };
            if mode >= 4 {
                let path = model_dir(&config, "test-model").join(&file.path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(partial_path(&path), b"abc").unwrap();
            }
            install_file(
                &config,
                &reqwest::Client::new(),
                "test-model",
                &file,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                fs::read(model_dir(&config, "test-model").join(&file.path)).unwrap(),
                b"abcdef"
            );
            assert_eq!(
                origin_calls.load(Ordering::Relaxed),
                match mode {
                    0 => 0,
                    3 => 2,
                    _ => 1,
                }
            );
            assert_eq!(mirror_calls.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn installation_space_accounts_for_verified_files_and_partial_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let model = Model {
            id: "test-model".into(),
            license: "MIT".into(),
            license_url: String::new(),
            files: vec![ModelFile {
                path: "model.onnx".into(),
                url: "http://127.0.0.1:1".into(),
                size: 6,
                sha256: format!("{:x}", Sha256::digest(b"abcdef")),
            }],
        };
        let models = [model];
        let path = model_dir(&config, "test-model").join("model.onnx");
        let stop = AtomicBool::new(false);
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 6);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(partial_path(&path), b"abc").unwrap();
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 3);
        fs::write(&path, b"xxxxxx").unwrap();
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 3);
        fs::write(partial_path(&path), b"xxxxxx").unwrap();
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 6);
        fs::write(partial_path(&path), b"abcdef").unwrap();
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 0);
        fs::write(&path, b"abcdef").unwrap();
        assert_eq!(pending_bytes(&config, &models, &stop).unwrap(), 0);
        assert!(check_space(0, 0).is_ok());
        assert!(check_space(6, 6 + 64 * 1024 * 1024).is_ok());
        let error = check_space(6, 6 + 64 * 1024 * 1024 - 1).unwrap_err();
        assert_eq!(
            error.downcast_ref::<DomainError>().unwrap().code,
            ErrorCode::SubtitleSpace
        );
    }

    #[test]
    fn vad_dependencies_are_independent_of_manifest_order() {
        let mut models = manifest();
        models.reverse();
        models.push(Model {
            id: "future-model".into(),
            license: "MIT".into(),
            license_url: String::new(),
            files: vec![],
        });
        let required = required_from(models, "qwen3-asr-0.6b-int8").unwrap();
        assert_eq!(required[0].id, "qwen3-asr-0.6b-int8");
        assert_eq!(required[1].id, "silero-vad");
    }

    #[test]
    fn list_requires_vad_and_a_shared_resource_lock() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let required = required_models("sensevoice-small-yue-int8").unwrap();
        let create = |model: &Model| {
            for file in &model.files {
                let path = model_dir(&config, &model.id).join(&file.path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                File::create(path).unwrap().set_len(file.size).unwrap();
            }
        };
        create(&required[0]);
        let listed = model_list(&config).unwrap();
        assert_eq!(listed[0]["present"], false);
        assert_eq!(
            listed[0]["bytes"],
            required
                .iter()
                .flat_map(|m| &m.files)
                .map(|f| f.size)
                .sum::<u64>()
        );
        create(&required[1]);
        assert_eq!(model_list(&config).unwrap()[0]["present"], true);
        let writer = lock(&config, true).unwrap();
        assert_eq!(
            model_list(&config)
                .unwrap_err()
                .downcast_ref::<DomainError>()
                .unwrap()
                .code,
            ErrorCode::SubtitleBusy
        );
        drop(writer);
    }

    #[test]
    fn validation_preserves_cancellation_and_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.onnx");
        fs::write(&path, b"abcdef").unwrap();
        let file = ModelFile {
            path: "model.onnx".into(),
            url: "http://127.0.0.1:1".into(),
            size: 6,
            sha256: format!("{:x}", Sha256::digest(b"abcdef")),
        };
        let stop = AtomicBool::new(true);
        assert_eq!(
            valid(&path, &file, &stop)
                .unwrap_err()
                .downcast_ref::<DomainError>()
                .unwrap()
                .code,
            ErrorCode::Cancelled
        );
        assert_eq!(
            download(&reqwest::Client::new(), "test-model", &path, &file, &stop)
                .unwrap_err()
                .downcast_ref::<DomainError>()
                .unwrap()
                .code,
            ErrorCode::Cancelled
        );
        assert!(valid(&path.join("child"), &file, &AtomicBool::new(false)).is_err());
        assert!(valid(&path, &file, &AtomicBool::new(false)).unwrap());
    }

    #[test]
    fn downloads_resume_only_a_matching_range_and_restart_on_full_response() {
        for ranged in [true, false] {
            let server = Server::new(move |request, stream| {
                assert!(request.to_ascii_lowercase().contains("range: bytes=3-"));
                if ranged {
                    stream.write_all(b"HTTP/1.1 206 OK\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef").unwrap();
                } else {
                    response(stream, 200, "application/octet-stream", b"abcdef");
                }
            });
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("model.onnx");
            fs::write(dir.path().join("model.onnx.part"), b"abc").unwrap();
            let file = ModelFile {
                path: "model.onnx".into(),
                url: server.url.clone(),
                size: 6,
                sha256: format!("{:x}", Sha256::digest(b"abcdef")),
            };
            download(
                &reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                "test-model",
                &path,
                &file,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(fs::read(path).unwrap(), b"abcdef");
        }
    }

    #[test]
    fn bad_range_truncation_digest_and_cancellation_never_publish() {
        for mode in 0..4 {
            let server = Server::new(move |_, stream| {
                match mode {
                0 => stream.write_all(b"HTTP/1.1 206 OK\r\nContent-Range: bytes 0-2/6\r\nContent-Length: 3\r\nConnection: close\r\n\r\ndef").unwrap(),
                1 => response(stream, 200, "application/octet-stream", b"abc"),
                _ => response(stream, 200, "application/octet-stream", b"xxxxxx"),
            }
            });
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("model.onnx");
            let partial = dir.path().join("model.onnx.part");
            fs::write(&partial, b"abc").unwrap();
            let file = ModelFile {
                path: "model.onnx".into(),
                url: server.url.clone(),
                size: 6,
                sha256: format!("{:x}", Sha256::digest(b"abcdef")),
            };
            assert!(
                download(
                    &reqwest::Client::builder()
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap(),
                    "test-model",
                    &path,
                    &file,
                    &AtomicBool::new(mode == 3)
                )
                .is_err()
            );
            assert!(!path.exists());
            if mode == 2 {
                assert!(!partial.exists());
            }
        }
    }

    #[test]
    fn resources_cannot_be_modified_during_recognition() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let reader = lock(&config, false).unwrap();
        assert!(lock(&config, true).is_err());
        assert!(lock(&config, false).is_ok());
        drop(reader);
        assert!(lock(&config, true).is_ok());
        assert!(required_models("../../outside").is_err());
    }
}
