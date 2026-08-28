//! Language dispatch: map a file extension to a `Lang`, with the right
//! tree-sitter grammar (or `None` for grammar-less languages handled by a small
//! line scanner). This keeps `repo_map`, `symbol_refs`, and `call_graph`
//! language-aware without each one re-deriving the extension table.
//!
//! Polyglot repos commonly mix C/C++, Rust, Python, C#, GLSL shaders, and
//! TypeScript/TSX/Svelte (plus plain `.js` / `.mjs` / `.cjs` via the TypeScript
//! grammar); some also ship Daslang (`.das`) scripts. C/C++, Rust,
//! Python, C#, GLSL, and TypeScript all have tree-sitter grammars, so they get exact
//! extraction (GLSL via `tree-sitter-glsl`, a `tree-sitter-c` fork sharing the C
//! node kinds; TSX and Svelte ride the TypeScript grammar). Svelte single-file
//! components keep their logic in `<script>` blocks, so [`Lang::preprocess`]
//! blanks everything else (newlines preserved) before parsing. Daslang's grammar
//! is large and vendored, so it is opt-in behind the `daslang` feature; without
//! it, Daslang falls back to a lightweight line scanner that still recovers
//! top-level decls.

use std::borrow::Cow;
use std::path::Path;
use tree_sitter::Language;

/// Binding to the vendored Daslang grammar (`vendor/tree-sitter-daslang/`,
/// compiled by build.rs). Present only with the `daslang` feature.
///
/// Explicit `#[link]` is required because this package's lib and bin share the
/// name `wordkeep`, so Cargo does not auto-link the bin against the lib — and
/// build-script `rustc-link-lib` alone then fails to reach the binary link line.
#[cfg(feature = "daslang")]
mod daslang_ffi {
    use tree_sitter_language::LanguageFn;

    #[link(name = "tree_sitter_daslang", kind = "static")]
    extern "C" {
        fn tree_sitter_daslang() -> *const ();
    }

    pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_daslang) };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Cpp,
    Rust,
    Python,
    Daslang,
    Glsl,
    Ts,
    Tsx,
    Svelte,
    CSharp,
}

impl Lang {
    /// Resolve a bare extension (no dot, any case) to a language.
    pub fn from_ext(ext: &str) -> Option<Lang> {
        let e = ext.to_ascii_lowercase();
        Some(match e.as_str() {
            "h" | "hpp" | "hh" | "cpp" | "cc" | "cxx" => Lang::Cpp,
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "das" => Lang::Daslang,
            "glsl" | "vert" | "frag" | "comp" | "geom" | "tesc" | "tese" | "vs" | "fs" => {
                Lang::Glsl
            }
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => Lang::Ts,
            "tsx" => Lang::Tsx,
            "svelte" => Lang::Svelte,
            "cs" => Lang::CSharp,
            _ => return None,
        })
    }

    /// Resolve a path by its extension.
    pub fn from_path(path: &Path) -> Option<Lang> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Lang::from_ext)
    }

    /// The tree-sitter grammar for this language, or `None` when it is handled
    /// by the line scanner (Daslang, unless the `daslang` feature is enabled).
    /// TSX and Svelte both parse with the TypeScript grammar - TSX uses the JSX
    /// dialect, and Svelte logic is isolated to `<script>` by [`Self::preprocess`].
    pub fn ts_language(self) -> Option<Language> {
        match self {
            Lang::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
            Lang::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Lang::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Lang::Glsl => Some(tree_sitter_glsl::LANGUAGE_GLSL.into()),
            Lang::Ts | Lang::Svelte => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Lang::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            Lang::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
            #[cfg(feature = "daslang")]
            Lang::Daslang => Some(daslang_ffi::LANGUAGE.into()),
            #[cfg(not(feature = "daslang"))]
            Lang::Daslang => None,
        }
    }

    /// Transform source before parsing. Identity for every language except
    /// Svelte, whose components mix markup and `<script>` logic - there we blank
    /// everything outside the script blocks so the TypeScript grammar parses a
    /// valid program while newlines (hence reported line numbers) are preserved.
    pub fn preprocess(self, raw: &str) -> Cow<'_, str> {
        match self {
            Lang::Svelte => Cow::Owned(svelte_script_only(raw)),
            _ => Cow::Borrowed(raw),
        }
    }
}

/// Blank every byte outside `<script>…</script>` regions, keeping newlines, so
/// the remaining buffer is the component's TypeScript at its original line
/// positions. Handles multiple script blocks (module + instance). The lowercase
/// copy used for matching preserves byte length, so offsets stay aligned.
fn svelte_script_only(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = bytes
        .iter()
        .map(|&b| if b == b'\n' { b'\n' } else { b' ' })
        .collect();
    let lower = raw.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(rel) = lower[search..].find("<script") {
        let open = search + rel;
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let content = open + gt + 1;
        let Some(crel) = lower[content..].find("</script>") else {
            break;
        };
        let cend = content + crel;
        out[content..cend].copy_from_slice(&bytes[content..cend]);
        search = cend + "</script>".len();
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn extensions_map_to_languages() {
        assert_eq!(Lang::from_ext("hpp"), Some(Lang::Cpp));
        assert_eq!(Lang::from_ext("CPP"), Some(Lang::Cpp)); // case-insensitive
        assert_eq!(Lang::from_ext("rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_ext("py"), Some(Lang::Python));
        assert_eq!(Lang::from_ext("das"), Some(Lang::Daslang));
        assert_eq!(Lang::from_ext("frag"), Some(Lang::Glsl));
        assert_eq!(Lang::from_ext("ts"), Some(Lang::Ts));
        assert_eq!(Lang::from_ext("mts"), Some(Lang::Ts));
        assert_eq!(Lang::from_ext("js"), Some(Lang::Ts));
        assert_eq!(Lang::from_ext("mjs"), Some(Lang::Ts));
        assert_eq!(Lang::from_ext("cjs"), Some(Lang::Ts));
        assert_eq!(Lang::from_ext("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_ext("svelte"), Some(Lang::Svelte));
        assert_eq!(Lang::from_ext("cs"), Some(Lang::CSharp));
        assert_eq!(Lang::from_ext("md"), None);
        assert_eq!(Lang::from_ext("txt"), None);
    }

    #[test]
    fn from_path_uses_extension() {
        assert_eq!(Lang::from_path(Path::new("src/a.cpp")), Some(Lang::Cpp));
        assert_eq!(Lang::from_path(Path::new("tools/x.rs")), Some(Lang::Rust));
        assert_eq!(Lang::from_path(Path::new("a/b/s.das")), Some(Lang::Daslang));
        assert_eq!(Lang::from_path(Path::new("README")), None);
    }

    #[test]
    fn grammar_presence_matches_language() {
        assert!(Lang::Cpp.ts_language().is_some());
        assert!(Lang::Rust.ts_language().is_some());
        assert!(Lang::Python.ts_language().is_some());
        assert!(Lang::Glsl.ts_language().is_some());
        assert!(Lang::Ts.ts_language().is_some());
        assert!(Lang::Tsx.ts_language().is_some());
        assert!(Lang::Svelte.ts_language().is_some());
        assert!(Lang::CSharp.ts_language().is_some());
        // Daslang grammar is opt-in behind the `daslang` feature.
        #[cfg(feature = "daslang")]
        assert!(Lang::Daslang.ts_language().is_some());
        #[cfg(not(feature = "daslang"))]
        assert!(Lang::Daslang.ts_language().is_none());
    }

    #[test]
    fn svelte_preprocess_keeps_only_script_and_preserves_lines() {
        let src = "<script lang=\"ts\">\nfunction greet() {}\n</script>\n<h1>hi {name}</h1>\n";
        let out = Lang::Svelte.preprocess(src);
        assert!(out.contains("function greet() {}"), "{out}");
        // Markup is blanked, not parsed as TS.
        assert!(!out.contains("<h1>"), "{out}");
        // Newlines preserved so line numbers still map to the .svelte file.
        assert_eq!(out.lines().count(), src.lines().count());
        assert_eq!(out.lines().nth(1), Some("function greet() {}"));
        // Non-Svelte languages are passed through untouched (borrowed).
        assert!(matches!(
            Lang::Ts.preprocess(src),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
