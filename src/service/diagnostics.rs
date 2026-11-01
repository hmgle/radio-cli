use crate::config::Config;
use anyhow::{Context, Result};
use std::{
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

pub(crate) fn doctor_report(
    config: &Config,
    stop: &Arc<AtomicBool>,
) -> Result<(serde_json::Value, bool)> {
    let mut checks = vec![];
    let mut failed = false;
    for (name, exe, flag) in [
        ("mpv", &config.settings.mpv, "--version"),
        ("ffmpeg", &config.settings.ffmpeg, "-version"),
        ("ffprobe", &config.settings.ffprobe, "-version"),
    ] {
        let result =
            crate::process::output(Command::new(exe).arg(flag), Duration::from_secs(5), stop);
        let (ok, detail, code) = match result {
            Ok(output) => (
                true,
                String::from_utf8_lossy(&output)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
                None,
            ),
            Err(e) => (
                false,
                e.to_string(),
                e.downcast_ref::<crate::error::DomainError>()
                    .map(|e| e.code.as_str()),
            ),
        };
        failed |= !ok;
        checks.push(serde_json::json!({"name":name,"ok":ok,"detail":detail,"code":code}));
    }
    std::fs::create_dir_all(&config.recording_dir)
        .context(crate::t!("cli_recording_dir_unavailable"))?;
    let test_path = config
        .recording_dir
        .join(format!(".doctor-{}", uuid::Uuid::new_v4()));
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&test_path);
    let writable_ok = writable.is_ok();
    failed |= !writable_ok;
    drop(writable);
    let _ = std::fs::remove_file(&test_path);
    checks.push(
        serde_json::json!({"name":"recording_dir","ok":writable_ok,"detail":config.recording_dir}),
    );
    let available = fs2::available_space(&config.recording_dir)?;
    let minimum = config
        .settings
        .recording_min_free_mb
        .saturating_mul(1024 * 1024);
    let enough = available >= minimum;
    failed |= !enough;
    checks.push(serde_json::json!({"name":"recording_space","ok":enough,
        "available_bytes":available,"minimum_bytes":minimum,
        "detail":crate::t!("cli_recording_space", "available" => available, "minimum" => minimum)}));
    let report = serde_json::json!({"checks":checks,"config":config.config_path,"database":config.data_dir.join("radio.db")});
    Ok((report, failed))
}
