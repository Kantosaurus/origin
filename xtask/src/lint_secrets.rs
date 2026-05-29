// SPDX-License-Identifier: Apache-2.0
//! Walks the workspace AST, flags any `#[derive(Debug)]` struct whose field
//! name matches `(?i)(key|token|password|auth|secret|credential)` unless the
//! field type contains `Secret<…>` or the field has a `#[redact]` attribute.

#![allow(
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    clippy::missing_errors_doc
)]

use std::path::PathBuf;

use clap::Args as ClapArgs;
use regex::Regex;
use syn::visit::Visit;
use syn::{ItemStruct, Meta, Type, TypePath};
use walkdir::WalkDir;

/// Arguments for the `lint-secrets` subcommand.
#[derive(Debug, ClapArgs)]
pub struct CliArgs {
    /// Path to scan. Defaults to the workspace root.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub use CliArgs as Args;

/// Run the lint. Returns the process exit code: `0` clean, `1` on violation.
#[must_use]
pub fn run(args: Args) -> i32 {
    let pat = Regex::new(r"(?i)(key|token|password|auth|secret|credential)").expect("compile regex");
    let mut violations: Vec<String> = Vec::new();

    let paths_to_scan: Vec<PathBuf> = if args.path.is_dir() {
        WalkDir::new(&args.path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
            .filter(|e| {
                !e.path()
                    .components()
                    .any(|c| matches!(c.as_os_str().to_str(), Some("target" | "fixtures")))
            })
            .map(walkdir::DirEntry::into_path)
            .collect()
    } else {
        vec![args.path]
    };

    for p in &paths_to_scan {
        let Ok(src) = std::fs::read_to_string(p) else {
            continue;
        };
        let Ok(ast) = syn::parse_file(&src) else {
            continue;
        };
        let mut v = LintVisitor {
            regex: &pat,
            path: p.clone(),
            violations: &mut violations,
        };
        v.visit_file(&ast);
    }

    if violations.is_empty() {
        0
    } else {
        for v in &violations {
            eprintln!("secret-redaction violation: {v}");
        }
        1
    }
}

struct LintVisitor<'a> {
    regex: &'a Regex,
    path: PathBuf,
    violations: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for LintVisitor<'_> {
    fn visit_item_struct(&mut self, s: &'ast ItemStruct) {
        let derives_debug = s.attrs.iter().any(|a| {
            matches!(&a.meta, Meta::List(ml)
                if ml.path.is_ident("derive")
                   && ml.tokens.to_string().split(',').any(|t| t.trim() == "Debug"))
        });
        if !derives_debug {
            return;
        }
        for field in &s.fields {
            let Some(name) = field.ident.as_ref() else {
                continue;
            };
            let name_s = name.to_string();
            if !self.regex.is_match(&name_s) {
                continue;
            }
            if has_redact_attr(&field.attrs) {
                continue;
            }
            if is_secret_type(&field.ty) {
                continue;
            }
            // Pre-filter: only flag string-like fields. Numeric counts
            // (`u32`/`u64`), CRDT key handles, and other non-string types
            // are not byte-leak surfaces.
            if !is_string_like(&field.ty) {
                continue;
            }
            // Pre-filter: URL-suffixed fields are not secrets.
            if name_s.ends_with("_url") || name_s == "url" {
                continue;
            }
            self.violations.push(format!(
                "{p}: struct `{ty}` field `{f}` looks secret-typed but is `{kind}`; \
                 wrap in `Secret<…>` or add `#[redact]`",
                p = self.path.display(),
                ty = s.ident,
                f = name_s,
                kind = quote_type(&field.ty),
            ));
        }
    }
}

fn has_redact_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("redact"))
}

fn is_secret_type(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, .. }) = ty {
        return path.segments.iter().any(|seg| seg.ident == "Secret");
    }
    false
}

/// String-like types where a stored secret could leak through `Debug`.
/// Matches `String`, `&str`, `&'a str`, `Box<str>`, `Cow<'_, str>`,
/// `Vec<u8>`, `Bytes`, and any `Option<…>` / `Box<…>` wrapping them.
fn is_string_like(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => is_string_like(&r.elem),
        Type::Path(TypePath { path, .. }) => {
            let Some(last) = path.segments.last() else {
                return false;
            };
            let name = last.ident.to_string();
            match name.as_str() {
                "String" | "str" | "Bytes" => true,
                "Vec" => extract_first_generic(last)
                    .is_some_and(|inner| matches!(&inner, Type::Path(p) if p.path.is_ident("u8"))),
                "Box" | "Cow" | "Option" => extract_first_generic(last).as_ref().is_some_and(is_string_like),
                _ => false,
            }
        }
        _ => false,
    }
}

fn extract_first_generic(seg: &syn::PathSegment) -> Option<Type> {
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
        for a in &args.args {
            if let syn::GenericArgument::Type(t) = a {
                return Some(t.clone());
            }
        }
    }
    None
}

fn quote_type(ty: &Type) -> String {
    use quote::ToTokens;
    let mut s = proc_macro2::TokenStream::new();
    ty.to_tokens(&mut s);
    s.to_string()
}
