#[tokio::main]
async fn main() {
    if let Err(error) = wordkeep_wiki::run().await {
        eprintln!("wordkeep-wiki: {error}");
        std::process::exit(1);
    }
}
