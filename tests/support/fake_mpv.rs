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
    let episode=args.iter().any(|s|s=="--pause=yes");
    let mut episode_eof=false;
    let mut paused = episode;
    for line in input.lines() {
        let Ok(line) = line else { break; };
        writeln!(log,"{line}").unwrap(); log.flush().unwrap();
        if line.contains("\"quit\"") { break; }
        if episode {
            let id=line.split("\"request_id\":").nth(1).and_then(|v|v.split('}').next()).and_then(|v|v.parse::<u64>().ok());
            if let Some(id)=id {let _=writeln!(stream,"{{\"request_id\":{id},\"error\":\"success\"}}");}
            if line.contains("\"loadfile\"") {
                episode_eof=line.contains("/episode-eof");
                let seekable=!line.contains("/noseek");
                let _=writeln!(stream,"{{\"event\":\"file-loaded\"}}\n{{\"event\":\"property-change\",\"name\":\"seekable\",\"data\":{seekable}}}");
                continue;
            }
            if line.contains("\"set_property\",\"pause\",false") {
                let _=writeln!(stream,"{{\"event\":\"playback-restart\"}}");
                if episode_eof {let _=writeln!(stream,"{{\"event\":\"end-file\",\"reason\":\"eof\"}}");}
                continue;
            }
        }
        let event = if line.contains("\"loadfile\"") {
            if args.iter().any(|s| s == "--audio-device=test/fail") {
                writeln!(stream, "{{\"event\":\"end-file\",\"reason\":\"error\",\"file_error\":\"audio output initialization failed\"}}").unwrap();
                break;
            }
            else if line.contains("/fail") { "{\"event\":\"end-file\",\"reason\":\"error\"}".to_owned() }
            else if line.contains("/live-eof") { "{\"event\":\"playback-restart\"}\n{\"event\":\"end-file\",\"reason\":\"eof\"}".to_owned() }
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
