#![allow(non_snake_case)]

mod cli;
mod resources;
mod statistics;
mod storage;

fn main() -> anyhow::Result<()> {
    cli::run()
}
