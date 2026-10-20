use std::{fs::{self, OpenOptions}, io::{BufRead, BufReader, Write}, os::unix::net::UnixListener};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--version") { println!("fake mpv"); return; }
    if args.iter().any(|s| s == "--af=help") {
        let exe = std::env::current_exe().unwrap();
        let mut log = OpenOptions::new().create(true).append(true).open(exe.with_extension("probes")).unwrap();
        writeln!(log, "filters").unwrap();
        if !exe.with_extension("no-normalization").exists() { println!("  dynaudnorm : Dynamic Audio Normalizer"); }
        return;
    }
    if args.iter().any(|s| s == "--audio-device=help") {
        println!("List of detected audio devices:\n  'auto' (Autoselect device)\n  'test/headphones' (Test headphones)");
        return;
    }
    let socket = args.iter().find_map(|s| s.strip_prefix("--input-ipc-server=")).unwrap();
    let path = std::env::current_exe().unwrap().with_extension("commands");
    let mut log = OpenOptions::new().create(true).append(true).open(path).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    let input = BufReader::new(stream.try_clone().unwrap());
    let mut volume = args.iter().find_map(|s| s.strip_prefix("--volume=")).unwrap_or("80").parse::<i32>().unwrap();
    let mut paused = false;
    for line in input.lines() {
        let Ok(line) = line else { break; };
        writeln!(log,"{line}").unwrap(); log.flush().unwrap();
        if line.contains("\"quit\"") { break; }
        let event = if line.contains("\"loadfile\"") {
            if args.iter().any(|s| s == "--audio-device=test/fail") {
                writeln!(stream, "{{\"event\":\"end-file\",\"reason\":\"error\",\"file_error\":\"audio output initialization failed\"}}").unwrap();
                break;
            }
            else if line.contains("/fail") { "{\"event\":\"end-file\",\"reason\":\"error\"}".to_owned() }
            else { "{\"event\":\"playback-restart\"}".to_owned() }
        } else if line.contains("\"cycle\",\"pause\"") {
            paused = !paused;
            format!("{{\"event\":\"property-change\",\"name\":\"pause\",\"data\":{paused}}}")
        } else if let Some(delta) = line.split("\"add\",\"volume\",").nth(1) {
            volume += delta.split(']').next().unwrap().parse::<i32>().unwrap();
            format!("{{\"event\":\"property-change\",\"name\":\"volume\",\"data\":{volume}}}")
        } else { continue; };
        if writeln!(stream,"{event}").is_err() { break; }
    }
    let _ = fs::remove_file(socket);
}
