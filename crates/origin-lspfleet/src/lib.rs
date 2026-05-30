// SPDX-License-Identifier: Apache-2.0
//! Registry and auto-install decisioning for a fleet of language servers.
//!
//! This crate maps a source file (by extension) or a language name to the
//! language server that should handle it, together with the shell command used
//! to install that server and the command used to launch it. It also provides
//! pure helpers to aggregate (sort and deduplicate) and summarize editor
//! diagnostics. The crate performs no I/O: the daemon is responsible for
//! actually downloading and spawning the servers, using the data exposed here.
#![forbid(unsafe_code)]

/// A language server entry: the language it serves, how to install it, how to
/// launch it, and the file extensions it claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspServer {
    /// Human-readable language name, lowercase (for example `"rust"`).
    pub language: &'static str,
    /// Stable identifier / binary name of the server (for example `"rust-analyzer"`).
    pub server_id: &'static str,
    /// Shell command that installs the server.
    pub install: &'static str,
    /// Shell command that launches the server in stdio mode.
    pub launch: &'static str,
    /// File extensions (without the leading dot) handled by this server.
    pub extensions: &'static [&'static str],
}

impl LspServer {
    /// Returns `true` if this server claims the given extension (case-insensitive
    /// on ASCII), where `ext` is given without a leading dot.
    #[must_use]
    pub fn handles_extension(&self, ext: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext))
    }
}

/// Severity level attached to a diagnostic, ordered from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// A hard error that typically blocks compilation.
    Error,
    /// A warning that does not block compilation.
    Warning,
    /// Informational message.
    Info,
    /// A hint or suggestion.
    Hint,
}

/// A single diagnostic emitted by a language server for a file location.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Diagnostic {
    /// Path of the file the diagnostic refers to.
    pub file: String,
    /// One-based line number.
    pub line: u32,
    /// One-based column number.
    pub col: u32,
    /// Severity of the diagnostic.
    pub severity: Severity,
    /// Human-readable diagnostic message.
    pub message: String,
    /// The server or tool that produced the diagnostic (for example `"rust-analyzer"`).
    pub source: String,
}

/// The static fleet registry. Each tuple maps to one [`LspServer`].
///
/// Fields are: language, server id, install command, launch command, extensions.
static REGISTRY: &[LspServer] = &[
    LspServer { language: "rust", server_id: "rust-analyzer", install: "rustup component add rust-analyzer", launch: "rust-analyzer", extensions: &["rs"] },
    LspServer { language: "go", server_id: "gopls", install: "go install golang.org/x/tools/gopls@latest", launch: "gopls", extensions: &["go"] },
    LspServer { language: "python", server_id: "pyright", install: "npm install -g pyright", launch: "pyright-langserver --stdio", extensions: &["py", "pyi"] },
    LspServer { language: "typescript", server_id: "typescript-language-server", install: "npm install -g typescript typescript-language-server", launch: "typescript-language-server --stdio", extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"] },
    LspServer { language: "c", server_id: "clangd", install: "apt-get install -y clangd", launch: "clangd", extensions: &["c", "h"] },
    LspServer { language: "cpp", server_id: "clangd-cpp", install: "apt-get install -y clangd", launch: "clangd", extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx"] },
    LspServer { language: "java", server_id: "jdtls", install: "brew install jdtls", launch: "jdtls", extensions: &["java"] },
    LspServer { language: "lua", server_id: "lua-language-server", install: "brew install lua-language-server", launch: "lua-language-server", extensions: &["lua"] },
    LspServer { language: "ruby", server_id: "solargraph", install: "gem install solargraph", launch: "solargraph stdio", extensions: &["rb", "erb"] },
    LspServer { language: "php", server_id: "intelephense", install: "npm install -g intelephense", launch: "intelephense --stdio", extensions: &["php"] },
    LspServer { language: "csharp", server_id: "omnisharp", install: "dotnet tool install -g omnisharp", launch: "omnisharp -lsp", extensions: &["cs"] },
    LspServer { language: "kotlin", server_id: "kotlin-language-server", install: "brew install kotlin-language-server", launch: "kotlin-language-server", extensions: &["kt", "kts"] },
    LspServer { language: "swift", server_id: "sourcekit-lsp", install: "xcode-select --install", launch: "sourcekit-lsp", extensions: &["swift"] },
    LspServer { language: "scala", server_id: "metals", install: "coursier install metals", launch: "metals", extensions: &["scala", "sc"] },
    LspServer { language: "haskell", server_id: "haskell-language-server", install: "ghcup install hls", launch: "haskell-language-server-wrapper --lsp", extensions: &["hs", "lhs"] },
    LspServer { language: "elixir", server_id: "elixir-ls", install: "mix escript.install hex elixir_ls", launch: "elixir-ls", extensions: &["ex", "exs"] },
    LspServer { language: "erlang", server_id: "erlang-ls", install: "rebar3 escriptize", launch: "erlang_ls", extensions: &["erl", "hrl"] },
    LspServer { language: "clojure", server_id: "clojure-lsp", install: "brew install clojure-lsp/brew/clojure-lsp-native", launch: "clojure-lsp", extensions: &["clj", "cljs", "cljc", "edn"] },
    LspServer { language: "dart", server_id: "dart-language-server", install: "dart pub global activate dart_language_server", launch: "dart language-server", extensions: &["dart"] },
    LspServer { language: "zig", server_id: "zls", install: "brew install zls", launch: "zls", extensions: &["zig"] },
    LspServer { language: "nim", server_id: "nimlsp", install: "nimble install nimlsp", launch: "nimlsp", extensions: &["nim", "nims"] },
    LspServer { language: "ocaml", server_id: "ocaml-lsp", install: "opam install ocaml-lsp-server", launch: "ocamllsp", extensions: &["ml", "mli"] },
    LspServer { language: "fsharp", server_id: "fsautocomplete", install: "dotnet tool install -g fsautocomplete", launch: "fsautocomplete", extensions: &["fs", "fsi", "fsx"] },
    LspServer { language: "julia", server_id: "julia-languageserver", install: "julia -e 'using Pkg; Pkg.add(\"LanguageServer\")'", launch: "julia --startup-file=no -e 'using LanguageServer; runserver()'", extensions: &["jl"] },
    LspServer { language: "r", server_id: "r-languageserver", install: "Rscript -e 'install.packages(\"languageserver\")'", launch: "R --slave -e 'languageserver::run()'", extensions: &["r"] },
    LspServer { language: "perl", server_id: "perlnavigator", install: "npm install -g perlnavigator-server", launch: "perlnavigator --stdio", extensions: &["pl", "pm"] },
    LspServer { language: "bash", server_id: "bash-language-server", install: "npm install -g bash-language-server", launch: "bash-language-server start", extensions: &["sh", "bash"] },
    LspServer { language: "html", server_id: "vscode-html-language-server", install: "npm install -g vscode-langservers-extracted", launch: "vscode-html-language-server --stdio", extensions: &["html", "htm"] },
    LspServer { language: "css", server_id: "vscode-css-language-server", install: "npm install -g vscode-langservers-extracted", launch: "vscode-css-language-server --stdio", extensions: &["css", "scss", "less"] },
    LspServer { language: "json", server_id: "vscode-json-language-server", install: "npm install -g vscode-langservers-extracted", launch: "vscode-json-language-server --stdio", extensions: &["json", "jsonc"] },
    LspServer { language: "yaml", server_id: "yaml-language-server", install: "npm install -g yaml-language-server", launch: "yaml-language-server --stdio", extensions: &["yaml", "yml"] },
    LspServer { language: "toml", server_id: "taplo", install: "cargo install taplo-cli --locked", launch: "taplo lsp stdio", extensions: &["toml"] },
    LspServer { language: "markdown", server_id: "marksman", install: "brew install marksman", launch: "marksman server", extensions: &["md", "markdown"] },
    LspServer { language: "vue", server_id: "vue-language-server", install: "npm install -g @vue/language-server", launch: "vue-language-server --stdio", extensions: &["vue"] },
    LspServer { language: "svelte", server_id: "svelteserver", install: "npm install -g svelte-language-server", launch: "svelteserver --stdio", extensions: &["svelte"] },
    LspServer { language: "astro", server_id: "astro-ls", install: "npm install -g @astrojs/language-server", launch: "astro-ls --stdio", extensions: &["astro"] },
    LspServer { language: "terraform", server_id: "terraform-ls", install: "brew install hashicorp/tap/terraform-ls", launch: "terraform-ls serve", extensions: &["tf", "tfvars"] },
    LspServer { language: "dockerfile", server_id: "dockerfile-language-server", install: "npm install -g dockerfile-language-server-nodejs", launch: "docker-langserver --stdio", extensions: &["dockerfile"] },
    LspServer { language: "sql", server_id: "sqls", install: "go install github.com/sqls-server/sqls@latest", launch: "sqls", extensions: &["sql"] },
    LspServer { language: "graphql", server_id: "graphql-lsp", install: "npm install -g graphql-language-service-cli", launch: "graphql-lsp server -m stream", extensions: &["graphql", "gql"] },
    LspServer { language: "vim", server_id: "vim-language-server", install: "npm install -g vim-language-server", launch: "vim-language-server --stdio", extensions: &["vim"] },
    LspServer { language: "tex", server_id: "texlab", install: "cargo install texlab --locked", launch: "texlab", extensions: &["tex", "bib"] },
    LspServer { language: "cmake", server_id: "cmake-language-server", install: "pip install cmake-language-server", launch: "cmake-language-server", extensions: &["cmake"] },
    LspServer { language: "groovy", server_id: "groovy-language-server", install: "brew install groovy-language-server", launch: "groovy-language-server", extensions: &["groovy", "gradle"] },
];

/// Returns the full static language-server registry.
///
/// The slice contains at least 40 entries covering the major languages.
#[must_use]
pub const fn registry() -> &'static [LspServer] {
    REGISTRY
}

/// Returns the first server that handles the given file extension (without the
/// leading dot), if any. Matching is case-insensitive on ASCII.
#[must_use]
pub fn server_for_extension(ext: &str) -> Option<&'static LspServer> {
    REGISTRY.iter().find(|s| s.handles_extension(ext))
}

/// Returns the server registered for the given language name, if any. Matching
/// is case-insensitive on ASCII.
#[must_use]
pub fn server_for_language(lang: &str) -> Option<&'static LspServer> {
    REGISTRY
        .iter()
        .find(|s| s.language.eq_ignore_ascii_case(lang))
}

/// Sorts diagnostics by `(file, line, severity)` and removes exact duplicates.
///
/// Two diagnostics are considered duplicates when every field is equal. The
/// returned vector is stable with respect to the chosen sort key.
#[must_use]
pub fn aggregate(mut diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diags.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.severity.cmp(&b.severity))
            .then(a.col.cmp(&b.col))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.message.cmp(&b.message))
    });
    diags.dedup();
    diags
}

/// Counts the number of error- and warning-severity diagnostics.
///
/// Returns `(errors, warnings)`. `Info` and `Hint` diagnostics are ignored.
#[must_use]
pub fn summary(diags: &[Diagnostic]) -> (u32, u32) {
    let mut errors: u32 = 0;
    let mut warnings: u32 = 0;
    for d in diags {
        match d.severity {
            Severity::Error => errors = errors.saturating_add(1),
            Severity::Warning => warnings = warnings.saturating_add(1),
            Severity::Info | Severity::Hint => {}
        }
    }
    (errors, warnings)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::{
        aggregate, registry, server_for_extension, server_for_language, summary, Diagnostic,
        Severity,
    };

    fn diag(file: &str, line: u32, col: u32, sev: Severity, msg: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_owned(),
            line,
            col,
            severity: sev,
            message: msg.to_owned(),
            source: "test".to_owned(),
        }
    }

    #[test]
    fn registry_has_at_least_40_entries_all_fields_non_empty() {
        let reg = registry();
        assert!(reg.len() >= 40, "registry has only {} entries", reg.len());
        for s in reg {
            assert!(!s.language.is_empty(), "empty language");
            assert!(!s.server_id.is_empty(), "empty server_id for {}", s.language);
            assert!(!s.install.is_empty(), "empty install for {}", s.server_id);
            assert!(!s.launch.is_empty(), "empty launch for {}", s.server_id);
            assert!(
                !s.extensions.is_empty(),
                "no extensions for {}",
                s.server_id
            );
            for e in s.extensions {
                assert!(!e.is_empty(), "empty extension in {}", s.server_id);
            }
        }
    }

    #[test]
    fn rust_extension_maps_to_rust_analyzer() {
        let s = server_for_extension("rs").unwrap();
        assert_eq!(s.server_id, "rust-analyzer");
        assert_eq!(s.language, "rust");
    }

    #[test]
    fn python_language_maps_to_pyright() {
        let s = server_for_language("python").unwrap();
        assert_eq!(s.server_id, "pyright");
    }

    #[test]
    fn extension_lookup_is_case_insensitive() {
        let upper = server_for_extension("RS").unwrap();
        let lower = server_for_extension("rs").unwrap();
        assert_eq!(upper.server_id, lower.server_id);
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert!(server_for_extension("xyznotreal").is_none());
        assert!(server_for_language("brainfuck").is_none());
    }

    #[test]
    fn aggregate_sorts_and_dedups() {
        let input = vec![
            diag("b.rs", 10, 1, Severity::Warning, "w"),
            diag("a.rs", 5, 2, Severity::Error, "boom"),
            diag("a.rs", 5, 2, Severity::Error, "boom"), // exact dup
            diag("a.rs", 1, 1, Severity::Hint, "h"),
        ];
        let out = aggregate(input);
        assert_eq!(out.len(), 3, "duplicate not removed");
        assert_eq!(out[0].file, "a.rs");
        assert_eq!(out[0].line, 1);
        assert_eq!(out[1].line, 5);
        assert_eq!(out[2].file, "b.rs");
    }

    #[test]
    fn aggregate_orders_severity_within_same_line() {
        let input = vec![
            diag("a.rs", 7, 1, Severity::Warning, "w"),
            diag("a.rs", 7, 1, Severity::Error, "e"),
        ];
        let out = aggregate(input);
        assert_eq!(out[0].severity, Severity::Error);
        assert_eq!(out[1].severity, Severity::Warning);
    }

    #[test]
    fn summary_counts_errors_and_warnings() {
        let diags = vec![
            diag("a.rs", 1, 1, Severity::Error, "e1"),
            diag("a.rs", 2, 1, Severity::Error, "e2"),
            diag("a.rs", 3, 1, Severity::Warning, "w1"),
            diag("a.rs", 4, 1, Severity::Info, "i1"),
            diag("a.rs", 5, 1, Severity::Hint, "h1"),
        ];
        assert_eq!(summary(&diags), (2, 1));
    }

    #[test]
    fn summary_empty_is_zero() {
        assert_eq!(summary(&[]), (0, 0));
    }

    #[test]
    fn severity_orders_error_before_hint() {
        assert!(Severity::Error < Severity::Warning);
        assert!(Severity::Warning < Severity::Info);
        assert!(Severity::Info < Severity::Hint);
    }
}
