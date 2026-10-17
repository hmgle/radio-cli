use clap::Parser;

fn main() {
    if let Err(error) = radio_cli::cli::run(radio_cli::cli::Cli::parse()) {
        eprintln!("radio: {}", radio_cli::clean_text(&format!("{error:#}")));
        std::process::exit(1);
    }
}
