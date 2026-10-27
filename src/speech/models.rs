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
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
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

#[derive(Debug, Deserialize, Serialize)]
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
    fs::create_dir_all(root(config))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root(config).join("resources.lock"))?;
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

pub(super) fn verify(config: &Config, id: &str, stop: &AtomicBool) -> Result<()> {
    for m in required_models(id)? {
        for file in &m.files {
            cancelled(stop)?;
            let path = model_dir(config, &m.id).join(&file.path);
            ensure!(
                valid(&path, file, stop)?,
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
    let mut have = fs::metadata(&partial).map_or(0, |m| m.len());
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
        let _ = fs::remove_file(&partial);
        bail!(
            DomainError::new(ErrorCode::SubtitleIntegrity)
                .with("id", "MODEL")
                .with("path", path.display())
        );
    }
    fs::rename(partial, path)?;
    Ok(())
}

fn partial_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.part",
        path.extension().unwrap_or_default().to_string_lossy()
    ))
}

fn model_list(config: &Config) -> Result<serde_json::Value> {
    let _lock = lock(config, false)?;
    let mut rows = Vec::new();
    for model in manifest().into_iter().filter(|m| m.id != "silero-vad") {
        let mut bytes = 0;
        let mut all_present = true;
        for required in required_models(&model.id)? {
            for file in &required.files {
                bytes += file.size;
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
                    let client = reqwest::Client::builder()
                        .user_agent(crate::USER_AGENT)
                        .no_gzip()
                        .connect_timeout(Duration::from_secs(config.settings.connect_timeout_secs))
                        .timeout(Duration::from_secs(1800))
                        .build()?;
                    for m in required {
                        for f in &m.files {
                            download(&client, &model_dir(config, &m.id).join(&f.path), f, stop)?;
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
    fn cancelled_runtime_checks_do_not_start_setup_or_mask_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::test_support::config(dir.path());
        let stop = std::sync::Arc::new(AtomicBool::new(true));
        for error in [
            runtime_check(&config, &stop).unwrap_err(),
            manage(&config, Action::Setup, &stop).unwrap_err(),
        ] {
            assert_eq!(
                error.downcast_ref::<DomainError>().unwrap().code,
                ErrorCode::Cancelled
            );
        }
        assert!(!root(&config).exists());
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
            download(&reqwest::Client::new(), &path, &file, &stop)
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
