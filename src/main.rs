use clap::Parser;

fn main() {
    let json_v2 = std::env::args_os()
        .skip(1)
        .take_while(|s| s != "--")
        .any(|s| s == "--json-v2" || s.to_str().is_some_and(|s| s.starts_with("--json-v2=")));
    let cli = match radio_cli::cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if json_v2 && error.use_stderr() {
                println!(
                    "{}",
                    serde_json::json!({"ok":false,"error":{
                        "code":"usage_error","message":error.to_string(),"params":{}
                    }})
                );
            } else {
                let _ = error.print();
            }
            std::process::exit(error.exit_code());
        }
    };
    if let Err(error) = radio_cli::cli::run(cli) {
        if json_v2 {
            println!("{}", radio_cli::error::json_error(&error));
        } else {
            eprintln!("radio: {}", radio_cli::clean_text(&format!("{error:#}")));
        }
        std::process::exit(radio_cli::error::exit_code(&error));
    }
}
