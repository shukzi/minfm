mod app;
mod archive;
mod block;
mod browser_loader;
mod cli;
mod config;
mod entry;
mod error;
mod icons;
mod launcher;
mod luks;
mod network;
mod operation;
mod partition;
mod process;
mod runtime;
mod safety;
#[allow(dead_code)] // Foundational API consumed by the following search slices.
mod search;
mod trash;
mod ui;
mod updater;

/// Runs minfm using the process arguments and terminal attached to this process.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    runtime::run(cli::parse(std::env::args_os().skip(1))?)
}
