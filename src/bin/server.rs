//! Standalone relay: builds without desktop/audio dependencies.

use argh::FromArgs;

#[derive(FromArgs)]
#[argh(description = "remember-me: standalone UDP voice relay")]
struct Opts {
    /// UDP listen port (default: 51900)
    #[argh(option, short = 'p', default = "51900")]
    port: u16,
}

#[tokio::main]
async fn main() {
    let opts: Opts = argh::from_env();
    if let Err(error) = remember_me::server::run_relay(opts.port).await {
        eprintln!("ретранслятор: {error}");
        std::process::exit(1);
    }
}
