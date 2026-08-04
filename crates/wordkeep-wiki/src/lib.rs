mod config;
mod dashboard;
mod garden;
mod indexer;
mod meili;
mod telemetry;
mod web;

use clap::{Parser, Subcommand};
use config::WikiConfig;
use meili::MeiliClient;
use serde_json::json;
use std::path::PathBuf;

pub use indexer::id_slug;
pub use web::{parse_bind, validate_page_path};

#[derive(Debug, Parser)]
#[command(
    name = "wordkeep-wiki",
    version,
    about = "Index and browse Wordkeep Markdown knowledge"
)]
struct Cli {
    /// Workspace root containing .wordkeep/config.json.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Index changed Markdown files into Meilisearch.
    Index {
        /// Reparse and replace every configured document.
        #[arg(long)]
        full: bool,
    },
    /// Watch configured document roots and index changes.
    Watch,
    /// Serve the local wiki API and static UI.
    Serve {
        /// Socket address. Defaults to config, WIKI_BIND, or 127.0.0.1:8787.
        #[arg(long)]
        bind: Option<String>,

        /// Watch and index document changes while serving.
        #[arg(long)]
        watch: bool,

        /// Permit binding to an address other than loopback.
        #[arg(long)]
        allow_non_loopback: bool,
    },
    /// Show Meilisearch and local manifest status.
    Status,
}

pub async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let root = cli
        .root
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", cli.root.display()))?;
    let config = WikiConfig::load(&root)?;
    let meili = MeiliClient::new(
        config.meili_url.clone(),
        config.meili_key.clone(),
        config.index_uid.clone(),
    )?;

    match cli.command {
        Command::Index { full } => {
            let summary = indexer::index_workspace(&root, &config, &meili, full).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&summary)
                    .map_err(|error| format!("serialize index summary: {error}"))?
            );
            Ok(())
        }
        Command::Watch => indexer::watch_workspace(root, config, meili).await,
        Command::Serve {
            bind,
            watch,
            allow_non_loopback,
        } => {
            let bind = bind.as_deref().unwrap_or(&config.bind).to_string();
            web::serve(root, config, meili, &bind, allow_non_loopback, watch).await
        }
        Command::Status => {
            let health = meili.health().await;
            let index = meili.index_stats().await;
            let manifest = indexer::load_manifest(&root)?;
            let chunks: usize = manifest.values().map(|entry| entry.chunk_ids.len()).sum();
            let output = json!({
                "root": root,
                "meilisearch": {
                    "health": result_value(health),
                    "index": result_value(index)
                },
                "manifest": {
                    "path": indexer::manifest_path(&root),
                    "files": manifest.len(),
                    "chunks": chunks
                }
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output)
                    .map_err(|error| format!("serialize status: {error}"))?
            );
            Ok(())
        }
    }
}

fn result_value(result: Result<serde_json::Value, String>) -> serde_json::Value {
    match result {
        Ok(value) => json!({"ok": true, "data": value}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}
