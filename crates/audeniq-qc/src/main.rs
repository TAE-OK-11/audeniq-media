fn main() {
    let code = audeniq_qc::cli::main("audeniq-qc", std::env::args_os().skip(1).collect());
    if code != 0 {
        std::process::exit(code);
    }
}
