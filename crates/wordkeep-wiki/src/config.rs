use serde_json::Value;
use std::path::{Component, Path, PathBuf};

const DEFAULT_DOC_ROOTS: [&str; 5] = [
    ".cursor/rules",
    "docs",
    ".github",
    "README.md",
    ".wordkeep/notes",
];

#[derive(Clone, Debug)]
pub(crate) struct WikiConfig {
    pub doc_roots: Vec<String>,
    pub meili_url: String,
    pub meili_key: Option<String>,
    pub index_uid: String,
    pub bind: String,
    /// When true, search telemetry may persist raw query strings (opt-in).
    pub retain_search_queries: bool,
    /// Display name for the wiki brand (`{project_name} ~ Wiki`).
    pub project_name: String,
    /// When true, block page writes (public demo hosts).
    pub read_only: bool,
}

impl WikiConfig {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(".wordkeep/config.json");
        let document = if path.exists() {
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|error| format!("parse {}: {error}", path.display()))?
        } else {
            Value::Object(Default::default())
        };

        let doc_roots = document
            .get("default_doc_roots")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|roots| !roots.is_empty())
            .unwrap_or_else(|| {
                DEFAULT_DOC_ROOTS
                    .iter()
                    .map(|root| (*root).to_string())
                    .collect()
            });

        for doc_root in &doc_roots {
            validate_doc_root(doc_root)?;
        }

        let wiki = document.get("wiki").and_then(Value::as_object);
        let configured = |key: &str| {
            wiki.and_then(|object| object.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let environment = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };

        let meili_url = environment("WIKI_MEILI_URL")
            .or_else(|| configured("meili_url"))
            .unwrap_or_else(|| "http://127.0.0.1:7700".to_string());
        let meili_key = environment("WIKI_MEILI_MASTER_KEY")
            .or_else(|| configured("meili_key"))
            .or_else(|| Some("wordkeep_dev_key".to_string()));
        let index_uid = configured("index").unwrap_or_else(|| "wiki_chunks".to_string());
        if !index_uid
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        {
            return Err(format!(
                "wiki index uid must contain only ASCII letters, digits, '-' or '_': {index_uid}"
            ));
        }
        let bind = environment("WIKI_BIND")
            .or_else(|| configured("bind"))
            .unwrap_or_else(|| "127.0.0.1:8787".to_string());
        let retain_search_queries = environment("WIKI_RETAIN_QUERIES")
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .or_else(|| {
                wiki.and_then(|object| object.get("retain_search_queries"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);
        let project_name = environment("WIKI_PROJECT_NAME")
            .or_else(|| configured("project_name"))
            .or_else(|| {
                document
                    .get("project_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| detect_project_name(root));
        let read_only = environment("WIKI_READ_ONLY")
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .or_else(|| {
                wiki.and_then(|object| object.get("read_only"))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false);

        Ok(Self {
            doc_roots,
            meili_url: meili_url.trim_end_matches('/').to_string(),
            meili_key,
            index_uid,
            bind,
            retain_search_queries,
            project_name,
            read_only,
        })
    }

    pub fn configured_path(&self, root: &Path, configured: &str) -> PathBuf {
        root.join(configured)
    }

    pub fn root_label_for(&self, relative_path: &str) -> Option<String> {
        self.doc_roots.iter().find_map(|configured| {
            let configured = configured.trim_end_matches('/');
            if relative_path == configured
                || relative_path
                    .strip_prefix(configured)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                Some(configured.to_string())
            } else {
                None
            }
        })
    }
}

fn detect_project_name(root: &Path) -> String {
    if let Some(name) = cmake_project_name(root) {
        return prettify_project_name(&name);
    }
    if let Some(name) = cargo_package_name(root) {
        return prettify_project_name(&name);
    }
    if let Some(name) = npm_package_name(root) {
        return prettify_project_name(&name);
    }
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(prettify_project_name)
        .unwrap_or_else(|| "Project".to_string())
}

fn cmake_project_name(root: &Path) -> Option<String> {
    let path = root.join("CMakeLists.txt");
    if !path.is_file() {
        return None;
    }
    let source = std::fs::read_to_string(path).ok()?;
    for line in source.lines() {
        let trimmed = line.trim().trim_end_matches('\r');
        let lower = trimmed.to_ascii_lowercase();
        let Some(project_at) = lower.find("project") else {
            continue;
        };
        let after_keyword = trimmed[project_at + "project".len()..].trim_start();
        let Some(after_paren) = after_keyword.strip_prefix('(').map(str::trim_start) else {
            continue;
        };
        let token = after_paren
            .split(|character: char| character.is_whitespace() || character == ')')
            .find(|part| !part.is_empty())?;
        if token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        {
            return Some(token.to_string());
        }
    }
    None
}

fn cargo_package_name(root: &Path) -> Option<String> {
    let source = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name") {
            let value = value.trim().trim_start_matches('=').trim();
            let value = value.trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn npm_package_name(root: &Path) -> Option<String> {
    let bytes = std::fs::read(root.join("package.json")).ok()?;
    let document: Value = serde_json::from_slice(&bytes).ok()?;
    document
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.rsplit('/').next().unwrap_or(value).to_string())
}

fn prettify_project_name(raw: &str) -> String {
    let spaced = raw.replace(['_', '-'], " ");
    let mut words = Vec::new();
    let mut current = String::new();
    for character in spaced.chars() {
        if character.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty()
            && character.is_ascii_uppercase()
            && current
                .chars()
                .last()
                .is_some_and(|previous| previous.is_ascii_lowercase() || previous.is_ascii_digit())
        {
            words.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.is_empty() {
        return "Project".to_string();
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.push_str(&chars.as_str().to_lowercase());
                    // Preserve common all-caps tokens after title-casing short words.
                    if word.len() <= 3 && word.chars().all(|c| c.is_ascii_uppercase()) {
                        word.to_string()
                    } else {
                        out
                    }
                }
                None => word,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_doc_root(value: &str) -> Result<(), String> {
    if value.contains('\\') || value.as_bytes().get(1) == Some(&b':') {
        return Err(format!("default_doc_roots entry must be relative: {value}"));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "default_doc_roots entry must not escape root: {value}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_labels_match_files_and_directories() {
        let config = WikiConfig {
            doc_roots: vec!["docs".into(), "README.md".into()],
            meili_url: String::new(),
            meili_key: None,
            index_uid: String::new(),
            bind: String::new(),
            retain_search_queries: false,
            project_name: "Demo".into(),
            read_only: false,
        };
        assert_eq!(config.root_label_for("docs/a.md").as_deref(), Some("docs"));
        assert_eq!(
            config.root_label_for("README.md").as_deref(),
            Some("README.md")
        );
        assert!(config.root_label_for("docs-old/a.md").is_none());
    }

    #[test]
    fn prettify_splits_snake_and_camel_names() {
        assert_eq!(prettify_project_name("BetwixtEngine"), "Betwixt Engine");
        assert_eq!(prettify_project_name("betwixt_engine"), "Betwixt Engine");
        assert_eq!(prettify_project_name("my-cool-app"), "My Cool App");
    }

    #[test]
    fn cmake_project_name_is_detected() {
        let dir = std::env::temp_dir().join(format!("wordkeep_wiki_cmake_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.25)\nproject(BetwixtEngine LANGUAGES C CXX)\n",
        )
        .unwrap();
        assert_eq!(detect_project_name(&dir), "Betwixt Engine");
        let _ = std::fs::remove_dir_all(dir);
    }
}
