use crate::config::WikiConfig;
use crate::meili::MeiliClient;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};
use wordkeep_knowledge::parse_markdown;

#[derive(Clone, Debug)]
pub(crate) struct DocFile {
    pub absolute: PathBuf,
    pub relative: String,
    pub root_label: String,
}

#[derive(Clone, Debug, Serialize)]
struct ChunkDocument {
    id: String,
    path: String,
    heading: String,
    heading_path: Vec<String>,
    heading_level: u8,
    anchor: String,
    body: String,
    content: String,
    kind: String,
    root: String,
    tags: Vec<String>,
    start_line: usize,
    end_line: usize,
    mtime_ns: u64,
    content_hash: String,
    updated_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ManifestEntry {
    pub mtime_ns: u64,
    pub chunk_ids: Vec<String>,
}

pub(crate) type Manifest = BTreeMap<String, ManifestEntry>;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct IndexSummary {
    pub scanned_files: usize,
    pub updated_files: usize,
    pub skipped_files: usize,
    pub deleted_files: usize,
    pub indexed_chunks: usize,
}

enum SyncResult {
    Skipped,
    Updated(usize),
}

pub(crate) async fn index_workspace(
    root: &Path,
    config: &WikiConfig,
    meili: &MeiliClient,
    full: bool,
) -> Result<IndexSummary, String> {
    meili.ensure_index().await?;
    let documents = collect_docs(root, config)?;
    let mut manifest = load_manifest(root)?;
    let mut summary = IndexSummary {
        scanned_files: documents.len(),
        ..IndexSummary::default()
    };

    for document in documents.values() {
        match sync_file(root, document, config, meili, &mut manifest, full).await? {
            SyncResult::Skipped => summary.skipped_files += 1,
            SyncResult::Updated(chunks) => {
                summary.updated_files += 1;
                summary.indexed_chunks += chunks;
            }
        }
    }

    let orphans: Vec<String> = manifest
        .keys()
        .filter(|path| !documents.contains_key(*path))
        .cloned()
        .collect();
    for orphan in orphans {
        if let Some(entry) = manifest.remove(&orphan) {
            meili.delete_documents(&entry.chunk_ids).await?;
            summary.deleted_files += 1;
        }
    }

    save_manifest(root, &manifest)?;
    Ok(summary)
}

async fn sync_file(
    root: &Path,
    document: &DocFile,
    _config: &WikiConfig,
    meili: &MeiliClient,
    manifest: &mut Manifest,
    force: bool,
) -> Result<SyncResult, String> {
    let metadata = std::fs::metadata(&document.absolute)
        .map_err(|error| format!("stat {}: {error}", document.absolute.display()))?;
    let mtime_ns = metadata_mtime_ns(&metadata);
    if !force
        && manifest
            .get(&document.relative)
            .is_some_and(|entry| entry.mtime_ns == mtime_ns)
    {
        return Ok(SyncResult::Skipped);
    }

    let source = std::fs::read_to_string(&document.absolute)
        .map_err(|error| format!("read {}: {error}", document.absolute.display()))?;
    let content_hash = sha256_hex(source.as_bytes());
    let parsed = parse_markdown(&document.relative, &source);
    let kind = kind_for_path(&document.relative);
    let updated_at = now_secs();
    let mut chunks = Vec::with_capacity(parsed.sections.len());

    for section in parsed.sections {
        let id = id_slug(&format!("{}#{}", document.relative, section.anchor));
        let content = if section.body.is_empty() {
            section.heading.clone()
        } else {
            format!("{}\n{}", section.heading, section.body)
        };
        chunks.push(ChunkDocument {
            id,
            path: document.relative.clone(),
            heading: section.heading,
            heading_path: section.heading_path,
            heading_level: section.heading_level,
            anchor: section.anchor,
            body: section.body,
            content,
            kind: kind.clone(),
            root: document.root_label.clone(),
            tags: parsed.tags.clone(),
            start_line: section.start_line,
            end_line: section.end_line,
            mtime_ns,
            content_hash: content_hash.clone(),
            updated_at,
        });
    }

    if let Some(old) = manifest.get(&document.relative) {
        meili.delete_documents(&old.chunk_ids).await?;
    }
    meili.upsert_documents(&chunks).await?;

    let chunk_ids = chunks.iter().map(|chunk| chunk.id.clone()).collect();
    manifest.insert(
        document.relative.clone(),
        ManifestEntry {
            mtime_ns,
            chunk_ids,
        },
    );
    let _ = root;
    Ok(SyncResult::Updated(chunks.len()))
}

pub(crate) fn collect_docs(
    root: &Path,
    config: &WikiConfig,
) -> Result<BTreeMap<String, DocFile>, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", root.display()))?;
    let mut documents = BTreeMap::new();

    for configured in &config.doc_roots {
        let base = config.configured_path(root, configured);
        if base.is_file() {
            add_doc(&canonical_root, &base, configured, &mut documents);
            continue;
        }
        if !base.is_dir() {
            continue;
        }

        let entries = WalkDir::new(&base)
            .follow_links(false)
            .into_iter()
            .filter_entry(should_descend)
            .filter_map(Result::ok);
        for entry in entries {
            if entry.file_type().is_file() {
                add_doc(&canonical_root, entry.path(), configured, &mut documents);
            }
        }
    }

    Ok(documents)
}

fn add_doc(
    canonical_root: &Path,
    path: &Path,
    configured_root: &str,
    documents: &mut BTreeMap<String, DocFile>,
) {
    if !is_markdown_path(path) {
        return;
    }
    let Ok(canonical) = path.canonicalize() else {
        return;
    };
    if !canonical.starts_with(canonical_root) {
        return;
    }
    let Ok(relative) = canonical.strip_prefix(canonical_root) else {
        return;
    };
    let relative = slash_path(relative);
    documents
        .entry(relative.clone())
        .or_insert_with(|| DocFile {
            absolute: canonical,
            relative,
            root_label: configured_root.trim_end_matches('/').to_string(),
        });
}

fn should_descend(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    !matches!(
        entry.file_name().to_string_lossy().as_ref(),
        ".git" | "target" | "node_modules"
    )
}

pub(crate) fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "mdc" | "markdown"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn kind_for_path(path: &str) -> String {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.starts_with(".cursor/rules/") {
        "rule"
    } else if normalized.starts_with(".wordkeep/notes/") {
        "note"
    } else if normalized.starts_with("tools/atheneum/") || normalized.contains("lore") {
        "lore"
    } else {
        "doc"
    }
    .to_string()
}

/// Produce a Meilisearch-safe stable identifier.
pub fn id_slug(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if separator && !output.is_empty() {
                output.push('-');
            }
            separator = false;
            output.push(character.to_ascii_lowercase());
        } else if character == '-' {
            if !output.is_empty() && !output.ends_with('-') {
                output.push('-');
            }
            separator = false;
        } else {
            separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "doc".to_string()
    } else {
        output
    }
}

pub(crate) async fn watch_workspace(
    root: PathBuf,
    config: WikiConfig,
    meili: MeiliClient,
) -> Result<(), String> {
    let summary = index_workspace(&root, &config, &meili, false).await?;
    if summary.updated_files > 0 || summary.deleted_files > 0 {
        eprintln!(
            "wordkeep-wiki: initial index: {} updated, {} skipped, {} deleted",
            summary.updated_files, summary.skipped_files, summary.deleted_files
        );
    }
    // Skip routine "0 updated, N skipped" chatter on healthy relaunches.

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| format!("create file watcher: {error}"))?;

    let watch_targets = watch_targets(&root, &config);
    for (target, recursive) in &watch_targets {
        watcher
            .watch(
                target,
                if *recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                },
            )
            .map_err(|error| format!("watch {}: {error}", target.display()))?;
    }
    if watch_targets.is_empty() {
        return Err("no existing document roots or parents are available to watch".to_string());
    }
    eprintln!("wordkeep-wiki: watching {} root(s)", watch_targets.len());

    loop {
        let first = tokio::select! {
            event = receiver.recv() => {
                event.ok_or_else(|| "file watcher channel closed".to_string())?
            }
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| format!("install Ctrl-C handler: {error}"))?;
                return Ok(());
            }
        };

        let mut paths = BTreeSet::new();
        collect_event_paths(first, &mut paths);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while let Ok(event) = receiver.try_recv() {
            collect_event_paths(event, &mut paths);
        }
        if let Err(error) = sync_changed_paths(&root, &config, &meili, paths).await {
            eprintln!("wordkeep-wiki: incremental update failed: {error}");
        }
    }
}

fn collect_event_paths(event: notify::Result<Event>, paths: &mut BTreeSet<PathBuf>) {
    match event {
        Ok(event) => paths.extend(event.paths),
        Err(error) => eprintln!("wordkeep-wiki: watch error: {error}"),
    }
}

fn watch_targets(root: &Path, config: &WikiConfig) -> BTreeMap<PathBuf, bool> {
    let mut targets = BTreeMap::new();
    for configured in &config.doc_roots {
        let path = config.configured_path(root, configured);
        if path.is_dir() {
            targets.insert(path, true);
            continue;
        }
        if path.is_file() {
            if let Some(parent) = path.parent() {
                targets.entry(parent.to_path_buf()).or_insert(false);
            }
            continue;
        }
        let mut parent = path.as_path();
        while !parent.exists() {
            let Some(next) = parent.parent() else {
                break;
            };
            parent = next;
        }
        if parent.starts_with(root) && parent.exists() {
            targets
                .entry(parent.to_path_buf())
                .and_modify(|recursive| *recursive = true)
                .or_insert(true);
        }
    }
    targets
}

async fn sync_changed_paths(
    root: &Path,
    config: &WikiConfig,
    meili: &MeiliClient,
    paths: BTreeSet<PathBuf>,
) -> Result<(), String> {
    let mut manifest = load_manifest(root)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", root.display()))?;
    let mut rescan = false;

    for event_path in paths {
        let absolute = if event_path.is_absolute() {
            event_path
        } else {
            root.join(event_path)
        };
        if absolute.is_dir() {
            rescan = true;
            continue;
        }
        let Ok(relative_path) = absolute.strip_prefix(root) else {
            continue;
        };
        let relative = slash_path(relative_path);

        if absolute.exists() && is_markdown_path(&absolute) {
            let Some(root_label) = config.root_label_for(&relative) else {
                continue;
            };
            let canonical = absolute
                .canonicalize()
                .map_err(|error| format!("canonicalize {}: {error}", absolute.display()))?;
            if !canonical.starts_with(&canonical_root) {
                continue;
            }
            let document = DocFile {
                absolute: canonical,
                relative,
                root_label,
            };
            let _ = sync_file(root, &document, config, meili, &mut manifest, false).await?;
        } else if !absolute.exists() {
            if is_markdown_path(&absolute) {
                delete_manifest_path(&relative, meili, &mut manifest).await?;
            } else {
                let prefix = format!("{}/", relative.trim_end_matches('/'));
                let removed: Vec<String> = manifest
                    .keys()
                    .filter(|path| path.starts_with(&prefix))
                    .cloned()
                    .collect();
                for path in removed {
                    delete_manifest_path(&path, meili, &mut manifest).await?;
                }
            }
        }
    }

    if rescan {
        let _ = index_workspace(root, config, meili, false).await?;
    } else {
        save_manifest(root, &manifest)?;
    }
    Ok(())
}

async fn delete_manifest_path(
    relative: &str,
    meili: &MeiliClient,
    manifest: &mut Manifest,
) -> Result<(), String> {
    if let Some(entry) = manifest.remove(relative) {
        meili.delete_documents(&entry.chunk_ids).await?;
    }
    Ok(())
}

pub(crate) fn load_manifest(root: &Path) -> Result<Manifest, String> {
    let path = manifest_path(root);
    if !path.exists() {
        return Ok(Manifest::new());
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn save_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    let path = manifest_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("serialize manifest: {error}"))?;
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("rename {}: {error}", path.display()))
}

pub(crate) fn manifest_path(root: &Path) -> PathBuf {
    global_cache_dir()
        .join("wordkeep")
        .join("workspaces")
        .join(workspace_id(root))
        .join("wiki_manifest.json")
}

pub(crate) fn global_cache_dir() -> PathBuf {
    wordkeep_knowledge::global_cache_dir()
}

fn workspace_id(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let normalized = canonical
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_slug_is_stable_and_meili_safe() {
        assert_eq!(
            id_slug("docs/My File.md#Hello, World!#2"),
            "docs-my-file-md-hello-world-2"
        );
        assert_eq!(id_slug("---"), "doc");
        assert!(id_slug("a/b_c").chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
        }));
    }

    #[test]
    fn kind_inference_follows_configured_conventions() {
        assert_eq!(kind_for_path(".cursor/rules/a.mdc"), "rule");
        assert_eq!(kind_for_path(".wordkeep/notes/a.md"), "note");
        assert_eq!(kind_for_path("tools/atheneum/world.md"), "lore");
        assert_eq!(kind_for_path("docs/lore/history.md"), "lore");
        assert_eq!(kind_for_path("docs/guide.md"), "doc");
    }
}
