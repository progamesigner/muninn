//! The `muninn` binary entrypoint: parse configuration, install tracing, build
//! the MCP server, and serve the selected transport until termination.

use clap::Parser;
use muninn::config::{Cli, Config};
use muninn::mcp::MuninnServer;
use muninn::transport;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config = match Config::from_cli_and_env(&cli) {
        Ok(config) => config,
        Err(err) => {
            // Fail fast with a single human-readable line to stderr.
            eprintln!("muninn: {err}");
            std::process::exit(1);
        }
    };

    // `--print-config`: dump the effective configuration to stderr and exit zero.
    if cli.print_config {
        eprintln!("{}", config.describe());
        return Ok(());
    }

    if let Err(err) = muninn::telemetry::init(&config.log_filter) {
        eprintln!("muninn: failed to initialise logging: {err}");
        std::process::exit(1);
    }

    let server = MuninnServer::new(&config);

    // Floor the worker count at two. The default follows the cgroup CPU quota, so
    // a container limited to 1 CPU gets a single worker — and one future that
    // blocks inline then halts the whole scheduler, including the liveness probe.
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().max(2));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    runtime.block_on(async move { transport::serve(&config, server).await })
}
