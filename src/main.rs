mod citations;
mod cli;
mod config;
mod discovery;
mod domain;
mod download;
mod nvidia;
mod pipeline;
mod qdrant;
mod quality;
mod render;
mod state;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
