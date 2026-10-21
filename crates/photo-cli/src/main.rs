#![forbid(unsafe_code)]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    photo_cli::main("audeniq-photo", std::env::args().skip(1).collect())
}
