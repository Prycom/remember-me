//! Standalone relay: builds without desktop/audio dependencies.

use argh::FromArgs;

#[derive(FromArgs)]
#[argh(description = "remember-me: standalone UDP voice relay")]
struct Opts {
    /// UDP listen port (default: 51900)
    #[argh(option, short = 'p', default = "51900")]
    port: u16,

    /// print every forwarded packet (default: signalling and summaries only)
    #[argh(switch)]
    verbose: bool,

    /// check bidirectional UDP connectivity to an updated relay at ip:port
    #[argh(option)]
    check_server: Option<String>,
}

#[tokio::main]
async fn main() {
    let opts: Opts = argh::from_env();
    let result = if let Some(target) = opts.check_server {
        remember_me::server::check_relay(&target).await
    } else {
        remember_me::server::run_relay_with_options(opts.port, opts.verbose).await
    };
    if let Err(error) = result {
        eprintln!("ретранслятор: {error}");
        std::process::exit(1);
    }
}
