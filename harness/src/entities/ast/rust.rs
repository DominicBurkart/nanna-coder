//! Rust AST entity
//!
//! A structured representation of a parsed Rust source file, produced by
//! walking the `syn` syntax tree. Unlike [`super::types::FileEntity`], which
//! stores raw file metadata plus a content preview, [`RustAstEntity`] stores a
//! language-aware summary (functions, structs, impl blocks, uses) so callers
//! can query a workspace symbolically without re-parsing.
//!
//! See issue #23 for the broader entity design.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors produced when parsing a source file into a [`RustAstEntity`].
#[derive(Error, Debug)]
pub enum AstError {
    /// Filesystem / IO failure reading the source file.
    #[error("IO error reading source file: {0}")]
    Io(#[from] std::io::Error),

    /// The source file could not be parsed as valid Rust.
    #[error("Syntax error parsing Rust source: {0}")]
    Syntax(String),

    /// The file extension / language is not supported by this parser.
    #[error("Unsupported language (expected Rust, got: {0})")]
    UnsupportedLanguage(String),
}

/// Signature of a top-level function declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionSig {
    /// Function name.
    pub name: String,
    /// Whether the function is declared `pub` (any visibility beyond private counts).
    pub is_pub: bool,
    /// Whether the function is declared `async`.
    pub is_async: bool,
    /// 1-indexed line number where the `fn` token appears.
    pub line: usize,
}

/// Signature of a top-level struct declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructSig {
    /// Struct name.
    pub name: String,
    /// Whether the struct is declared with any non-private visibility.
    pub is_pub: bool,
    /// 1-indexed line number where the `struct` token appears.
    pub line: usize,
}

/// Signature of an `impl` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplSig {
    /// Name of the concrete type being implemented (`Self` type).
    pub type_name: String,
    /// If this is a trait impl, the trait being implemented.
    pub trait_name: Option<String>,
    /// 1-indexed line number where the `impl` token appears.
    pub line: usize,
}

/// Structured, language-aware summary of a Rust source file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstSummary {
    /// All top-level function declarations discovered in the file.
    pub functions: Vec<FunctionSig>,
    /// All top-level struct declarations discovered in the file.
    pub structs: Vec<StructSig>,
    /// All `impl` blocks (inherent and trait) discovered in the file.
    pub impls: Vec<ImplSig>,
    /// All `use` statements, rendered in their canonical path form
    /// (e.g. `std::collections::HashMap`).
    pub uses: Vec<String>,
}

/// A parsed Rust source file as an [`Entity`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustAstEntity {
    /// Common entity metadata (id, timestamps, tags, ...).
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    /// Path to the source file that was parsed.
    pub path: PathBuf,
    /// Structured summary of the file's top-level items.
    pub summary: AstSummary,
    /// Stable content hash (FNV-1a 64) of the source bytes the summary was derived from.
    pub source_hash: u64,
}

impl RustAstEntity {
    /// Construct a new entity from an already-computed summary and source.
    pub fn new(path: PathBuf, summary: AstSummary, source: &str) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Ast),
            path,
            summary,
            source_hash: fnv1a_64(source.as_bytes()),
        }
    }
}

#[async_trait]
impl Entity for RustAstEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

/// Parse a single Rust source file into a [`RustAstEntity`].
///
/// Returns [`AstError::UnsupportedLanguage`] if the path does not have a `.rs`
/// extension, [`AstError::Io`] if the file cannot be read, and
/// [`AstError::Syntax`] if `syn` rejects the source.
pub fn parse_rust_file(path: &Path) -> Result<RustAstEntity, AstError> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => {}
        Some(other) => return Err(AstError::UnsupportedLanguage(other.to_string())),
        None => return Err(AstError::UnsupportedLanguage("(no extension)".to_string())),
    }

    let source = std::fs::read_to_string(path)?;
    let summary = parse_rust_source(&source)?;
    Ok(RustAstEntity::new(path.to_path_buf(), summary, &source))
}

/// Parse a Rust source string into an [`AstSummary`].
///
/// This is factored out so benchmarks and tests can drive the parser without
/// hitting the filesystem.
pub fn parse_rust_source(source: &str) -> Result<AstSummary, AstError> {
    let file = syn::parse_file(source).map_err(|e| AstError::Syntax(e.to_string()))?;

    let mut summary = AstSummary::default();

    for item in &file.items {
        match item {
            syn::Item::Fn(f) => {
                summary.functions.push(FunctionSig {
                    name: f.sig.ident.to_string(),
                    is_pub: is_visible(&f.vis),
                    is_async: f.sig.asyncness.is_some(),
                    line: line_of(&f.sig.fn_token),
                });
            }
            syn::Item::Struct(s) => {
                summary.structs.push(StructSig {
                    name: s.ident.to_string(),
                    is_pub: is_visible(&s.vis),
                    line: line_of(&s.struct_token),
                });
            }
            syn::Item::Impl(i) => {
                let type_name = type_path_string(&i.self_ty);
                let trait_name = i.trait_.as_ref().map(|(_, path, _)| path_to_string(path));
                summary.impls.push(ImplSig {
                    type_name,
                    trait_name,
                    line: line_of(&i.impl_token),
                });
            }
            syn::Item::Use(u) => {
                collect_use_paths(&u.tree, String::new(), &mut summary.uses);
            }
            _ => {}
        }
    }

    Ok(summary)
}

fn is_visible(vis: &syn::Visibility) -> bool {
    !matches!(vis, syn::Visibility::Inherited)
}

fn line_of<T: syn::spanned::Spanned>(spanned: &T) -> usize {
    spanned.span().start().line
}

fn type_path_string(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(tp) => path_to_string(&tp.path),
        syn::Type::Reference(r) => format!("&{}", type_path_string(&r.elem)),
        syn::Type::Tuple(t) if t.elems.is_empty() => "()".to_string(),
        _ => "<unknown>".to_string(),
    }
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn collect_use_paths(tree: &syn::UseTree, prefix: String, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => {
            let next = join_path(&prefix, &p.ident.to_string());
            collect_use_paths(&p.tree, next, out);
        }
        syn::UseTree::Name(n) => {
            out.push(join_path(&prefix, &n.ident.to_string()));
        }
        syn::UseTree::Rename(r) => {
            out.push(join_path(&prefix, &r.ident.to_string()));
        }
        syn::UseTree::Glob(_) => {
            out.push(join_path(&prefix, "*"));
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                collect_use_paths(item, prefix.clone(), out);
            }
        }
    }
}

fn join_path(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{}::{}", prefix, segment)
    }
}

/// FNV-1a 64-bit hash. Stable, non-cryptographic, and dependency-free so
/// `source_hash` values are deterministic across builds.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_rs(contents: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile()
            .expect("create temp .rs file");
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_parse_rust_file_simple_fn() {
        let f = write_rs("fn hello() {}\n");
        let entity = parse_rust_file(f.path()).expect("parse");
        assert_eq!(entity.summary.functions.len(), 1);
        let fn_sig = &entity.summary.functions[0];
        assert_eq!(fn_sig.name, "hello");
        assert!(!fn_sig.is_pub);
        assert!(!fn_sig.is_async);
        assert_eq!(fn_sig.line, 1);
    }

    #[test]
    fn test_parse_rust_file_pub_async_fn() {
        let f = write_rs("\n\npub async fn fetch() {}\n");
        let entity = parse_rust_file(f.path()).expect("parse");
        assert_eq!(entity.summary.functions.len(), 1);
        let fn_sig = &entity.summary.functions[0];
        assert_eq!(fn_sig.name, "fetch");
        assert!(fn_sig.is_pub);
        assert!(fn_sig.is_async);
        assert_eq!(fn_sig.line, 3);
    }

    #[test]
    fn test_parse_rust_file_struct() {
        let f = write_rs("pub struct Point { x: i32, y: i32 }\nstruct Priv;\n");
        let entity = parse_rust_file(f.path()).expect("parse");
        assert_eq!(entity.summary.structs.len(), 2);
        assert_eq!(entity.summary.structs[0].name, "Point");
        assert!(entity.summary.structs[0].is_pub);
        assert_eq!(entity.summary.structs[0].line, 1);
        assert_eq!(entity.summary.structs[1].name, "Priv");
        assert!(!entity.summary.structs[1].is_pub);
    }

    #[test]
    fn test_parse_rust_file_impl_inherent() {
        let f = write_rs("struct Point;\nimpl Point {\n  fn new() -> Self { Point }\n}\n");
        let entity = parse_rust_file(f.path()).expect("parse");
        assert_eq!(entity.summary.impls.len(), 1);
        let i = &entity.summary.impls[0];
        assert_eq!(i.type_name, "Point");
        assert!(i.trait_name.is_none());
        assert_eq!(i.line, 2);
    }

    #[test]
    fn test_parse_rust_file_impl_trait() {
        let src = "struct P;\nimpl std::fmt::Display for P {\n  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { Ok(()) }\n}\n";
        let f = write_rs(src);
        let entity = parse_rust_file(f.path()).expect("parse");
        assert_eq!(entity.summary.impls.len(), 1);
        let i = &entity.summary.impls[0];
        assert_eq!(i.type_name, "P");
        assert_eq!(i.trait_name.as_deref(), Some("std::fmt::Display"));
        assert_eq!(i.line, 2);
    }

    #[test]
    fn test_parse_rust_file_use_statement() {
        let src = "use std::collections::HashMap;\nuse std::{fs, io::Write};\nuse serde::Deserialize as De;\nuse std::path::*;\n";
        let f = write_rs(src);
        let entity = parse_rust_file(f.path()).expect("parse");
        assert!(entity
            .summary
            .uses
            .contains(&"std::collections::HashMap".to_string()));
        assert!(entity.summary.uses.contains(&"std::fs".to_string()));
        assert!(entity.summary.uses.contains(&"std::io::Write".to_string()));
        assert!(entity
            .summary
            .uses
            .contains(&"serde::Deserialize".to_string()));
        assert!(entity.summary.uses.contains(&"std::path::*".to_string()));
    }

    #[test]
    fn test_parse_rust_file_syntax_error() {
        let f = write_rs("fn broken( {}\n");
        let err = parse_rust_file(f.path()).expect_err("should fail");
        assert!(matches!(err, AstError::Syntax(_)));
    }

    #[test]
    fn test_parse_rust_file_missing_file() {
        let path = std::path::Path::new("/nonexistent/definitely/not/here.rs");
        let err = parse_rust_file(path).expect_err("should fail");
        assert!(matches!(err, AstError::Io(_)));
    }

    #[test]
    fn test_parse_rust_file_unsupported_language() {
        let mut f = tempfile::Builder::new()
            .suffix(".py")
            .tempfile()
            .expect("tmp .py");
        f.write_all(b"def foo(): pass\n").unwrap();
        f.flush().unwrap();
        let err = parse_rust_file(f.path()).expect_err("should fail");
        assert!(matches!(err, AstError::UnsupportedLanguage(_)));
    }

    #[tokio::test]
    async fn test_rust_ast_entity_implements_entity() {
        let f = write_rs("pub fn a() {}\npub struct S;\n");
        let entity = parse_rust_file(f.path()).expect("parse");

        assert_eq!(entity.entity_type(), EntityType::Ast);
        assert!(!entity.id().is_empty());

        let json = entity.to_json().expect("to_json");
        let round: RustAstEntity = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(round.source_hash, entity.source_hash);
        assert_eq!(round.summary, entity.summary);
        assert_eq!(round.path, entity.path);
        assert_eq!(round.metadata.id, entity.metadata.id);
    }

    #[test]
    fn test_ast_error_display() {
        let io_err = AstError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        assert!(io_err.to_string().contains("IO error"));

        let syn_err = AstError::Syntax("unexpected token".to_string());
        assert!(syn_err.to_string().contains("Syntax error"));

        let lang_err = AstError::UnsupportedLanguage("py".to_string());
        let s = lang_err.to_string();
        assert!(s.contains("Unsupported"));
        assert!(s.contains("py"));
    }

    #[test]
    fn test_ast_summary_serde_roundtrip() {
        let summary = AstSummary {
            functions: vec![FunctionSig {
                name: "f".to_string(),
                is_pub: true,
                is_async: false,
                line: 10,
            }],
            structs: vec![StructSig {
                name: "S".to_string(),
                is_pub: false,
                line: 20,
            }],
            impls: vec![ImplSig {
                type_name: "S".to_string(),
                trait_name: Some("Trait".to_string()),
                line: 30,
            }],
            uses: vec!["std::fs".to_string()],
        };

        let json = serde_json::to_string(&summary).expect("serialize");
        let back: AstSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }

    #[test]
    fn test_source_hash_is_stable() {
        let src = "fn x() {}\n";
        let a = parse_rust_source(src).unwrap();
        let entity_a = RustAstEntity::new(PathBuf::from("a.rs"), a.clone(), src);
        let entity_b = RustAstEntity::new(PathBuf::from("b.rs"), a, src);
        assert_eq!(entity_a.source_hash, entity_b.source_hash);
    }

    #[test]
    fn test_parse_rust_file_no_extension() {
        // Cover the `None =>` arm of the extension match in parse_rust_file.
        let f = tempfile::Builder::new()
            .prefix("noext")
            .suffix("")
            .tempfile()
            .expect("tmp no-ext");
        // Rebind to a path without any extension.
        let path = f.path().with_extension("");
        std::fs::write(&path, b"fn main() {}\n").unwrap();
        let err = parse_rust_file(&path).expect_err("should fail");
        match err {
            AstError::UnsupportedLanguage(s) => assert!(s.contains("no extension")),
            other => panic!("expected UnsupportedLanguage, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_type_path_string_reference_tuple_and_unknown() {
        // `impl X for &Foo` exercises `syn::Type::Reference`.
        let src = "struct Foo;\ntrait X {}\nimpl X for &Foo {}\n";
        let summary = parse_rust_source(src).expect("parse");
        assert_eq!(summary.impls.len(), 1);
        assert_eq!(summary.impls[0].type_name, "&Foo");

        // `impl X for ()` exercises the empty-tuple arm.
        let src = "trait X {}\nimpl X for () {}\n";
        let summary = parse_rust_source(src).expect("parse");
        assert_eq!(summary.impls.len(), 1);
        assert_eq!(summary.impls[0].type_name, "()");

        // `impl X for [u8; 4]` falls through to the `_ => "<unknown>"` arm
        // because `syn::Type::Array` is not matched.
        let src = "trait X {}\nimpl X for [u8; 4] {}\n";
        let summary = parse_rust_source(src).expect("parse");
        assert_eq!(summary.impls.len(), 1);
        assert_eq!(summary.impls[0].type_name, "<unknown>");
    }

    #[test]
    fn test_metadata_mut_is_exposed() {
        // Cover `Entity::metadata_mut` on RustAstEntity.
        let mut entity =
            RustAstEntity::new(PathBuf::from("x.rs"), AstSummary::default(), "fn x() {}\n");
        let original_id = entity.metadata.id.clone();
        {
            let m = entity.metadata_mut();
            m.tags.push("ast".to_string());
        }
        assert_eq!(entity.metadata.id, original_id);
        assert_eq!(entity.metadata().tags, vec!["ast".to_string()]);
    }

    #[test]
    fn test_parse_rust_source_direct() {
        // Drive `parse_rust_source` without the filesystem wrapper.
        let summary = parse_rust_source("pub fn top() {}\n").expect("parse");
        assert_eq!(summary.functions.len(), 1);
        assert!(summary.functions[0].is_pub);

        // Syntax error propagation from `parse_rust_source`.
        let err = parse_rust_source("fn (( {}\n").expect_err("should fail");
        assert!(matches!(err, AstError::Syntax(_)));
    }
}
