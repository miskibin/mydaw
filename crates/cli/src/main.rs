fn main() -> std::process::ExitCode {
    wave_cli::run_cli(std::env::args().skip(1).collect())
}
