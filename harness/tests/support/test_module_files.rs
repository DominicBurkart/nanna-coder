use std::collections::BTreeSet;
use std::path::Path;
use syn::visit::Visit;

pub fn join_path(parts: &[String]) -> String {
    let kept: Vec<String> = parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect();
    kept.join("/")
}

pub fn module_base(file: &str) -> String {
    let path = Path::new(file);
    let dir = path.parent().unwrap().to_string_lossy().replace('\\', "/");
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let owns_dir = !matches!(stem.as_str(), "lib" | "main" | "mod");
    join_path(&[dir, if owns_dir { stem } else { String::new() }])
}

fn is_exact_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| matches!(&meta, syn::Meta::Path(path) if path.is_ident("test")))
                .unwrap_or(false)
    })
}

pub fn is_exact_test_file_module(module: &syn::ItemMod) -> bool {
    module.content.is_none() && is_exact_cfg_test(&module.attrs)
}

pub fn cfg_test_file_modules(file: &str, source: &str) -> Vec<String> {
    let base = module_base(file);
    let mut skipped = BTreeSet::new();
    let mut kept = BTreeSet::new();
    for item in syn::parse_file(source).unwrap().items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        if is_exact_test_file_module(&module) {
            skipped.insert(name);
        } else {
            kept.insert(name);
        }
    }
    let mut files = Vec::new();
    for name in skipped.difference(&kept) {
        files.push(join_path(&[base.clone(), format!("{name}.rs")]));
        files.push(join_path(&[base.clone(), format!("{name}/mod.rs")]));
    }
    files
}

fn assigns_path(tokens: proc_macro2::TokenStream) -> bool {
    let mut previous_path = false;
    for token in tokens {
        match token {
            proc_macro2::TokenTree::Group(group) => {
                if assigns_path(group.stream()) {
                    return true;
                }
                previous_path = false;
            }
            proc_macro2::TokenTree::Ident(ident) => previous_path = ident == "path",
            proc_macro2::TokenTree::Punct(punct) => {
                if previous_path && punct.as_char() == '=' {
                    return true;
                }
                previous_path = false;
            }
            proc_macro2::TokenTree::Literal(_) => previous_path = false,
        }
    }
    false
}

fn attribute_assigns_path(tokens: proc_macro2::TokenStream) -> bool {
    let mut after_hash = false;
    for token in tokens {
        match token {
            proc_macro2::TokenTree::Group(group) => {
                let attribute = after_hash && group.delimiter() == proc_macro2::Delimiter::Bracket;
                if (attribute && assigns_path(group.stream()))
                    || attribute_assigns_path(group.stream())
                {
                    return true;
                }
                after_hash = false;
            }
            proc_macro2::TokenTree::Punct(punct) => {
                after_hash = punct.as_char() == '#' || (after_hash && punct.as_char() == '!');
            }
            _ => after_hash = false,
        }
    }
    false
}

#[derive(Default)]
struct PathAttributes {
    found: bool,
}

impl<'ast> Visit<'ast> for PathAttributes {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        let direct = node.path().is_ident("path");
        let conditional = node.path().is_ident("cfg_attr") && assigns_path(node_tokens(node));
        self.found = self.found || direct || conditional;
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.found = self.found || attribute_assigns_path(node.tokens.clone());
    }
}

fn node_tokens(attr: &syn::Attribute) -> proc_macro2::TokenStream {
    let list = attr.meta.require_list();
    list.map(|list| list.tokens.clone()).unwrap_or_default()
}

pub fn path_attribute_files(sources: &[(String, String)]) -> Vec<String> {
    let mut files = Vec::new();
    for (file, source) in sources {
        let mut visitor = PathAttributes::default();
        visitor.visit_file(&syn::parse_file(source).unwrap());
        if visitor.found {
            files.push(file.clone());
        }
    }
    files
}

pub fn skipped_files(sources: &[(String, String)]) -> Result<BTreeSet<String>, String> {
    let carriers = path_attribute_files(sources);
    if !carriers.is_empty() {
        return Err(format!(
            "cannot reason about module files: a path attribute (#[path], cfg_attr(.., path = ..) or one inside a macro) appears in {carriers:?}; review by hand before any cfg(test) file module is skipped"
        ));
    }
    let skipped = sources
        .iter()
        .flat_map(|(file, source)| cfg_test_file_modules(file, source))
        .collect::<BTreeSet<String>>();
    Ok(skipped)
}
