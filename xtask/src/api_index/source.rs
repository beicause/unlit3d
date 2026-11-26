//! Source locations and declaration signatures, without function bodies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quote::{ToTokens, quote};
use serde_json::Value;

pub(super) struct Sources<'a> {
    root: &'a Path,
    files: BTreeMap<PathBuf, Option<String>>,
}

impl<'a> Sources<'a> {
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: BTreeMap::new(),
        }
    }

    pub(super) fn declaration(
        &mut self,
        span: &Value,
        kind: &str,
    ) -> Option<(String, u64, Option<String>)> {
        let file = Path::new(span["filename"].as_str()?);
        let relative = if file.is_absolute() {
            file.strip_prefix(self.root).ok()?
        } else {
            file
        };
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        let first = span["begin"][0].as_u64()?;
        let text = self
            .files
            .entry(relative.to_owned())
            .or_insert_with(|| std::fs::read_to_string(self.root.join(relative)).ok())
            .as_deref()?;
        let signature = snippet(text, span).and_then(|text| signature(text, kind));
        Some((relative.to_str()?.to_owned(), first, signature))
    }
}

fn snippet<'a>(source: &'a str, span: &Value) -> Option<&'a str> {
    let position = |coords: &Value| {
        let line = usize::try_from(coords[0].as_u64()?).ok()?.checked_sub(1)?;
        let column = usize::try_from(coords[1].as_u64()?).ok()?.checked_sub(1)?;
        let mut offset = 0;
        let text = source
            .split_inclusive('\n')
            .inspect(|text| offset += text.len())
            .nth(line)?;
        let at = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .nth(column)?;
        Some(offset - text.len() + at)
    };
    source.get(position(&span["begin"])?..position(&span["end"])?)
}

fn signature(source: &str, kind: &str) -> Option<String> {
    let tokens = match kind {
        "function" => {
            if let Ok(item) = syn::parse_str::<syn::ItemFn>(source) {
                let (vis, sig) = (item.vis, item.sig);
                quote!(#vis #sig)
            } else if let Ok(item) = syn::parse_str::<syn::ImplItemFn>(source) {
                let (vis, sig) = (item.vis, item.sig);
                quote!(#vis #sig)
            } else {
                syn::parse_str::<syn::TraitItemFn>(source)
                    .ok()?
                    .sig
                    .into_token_stream()
            }
        }
        "struct" => {
            let item = syn::parse_str::<syn::ItemStruct>(source).ok()?;
            let (vis, ident, generics) = (item.vis, item.ident, item.generics);
            let predicates = &generics.where_clause;
            quote!(#vis struct #ident #generics #predicates)
        }
        "enum" => {
            let item = syn::parse_str::<syn::ItemEnum>(source).ok()?;
            let (vis, ident, generics) = (item.vis, item.ident, item.generics);
            let predicates = &generics.where_clause;
            quote!(#vis enum #ident #generics #predicates)
        }
        "union" => {
            let item = syn::parse_str::<syn::ItemUnion>(source).ok()?;
            let (vis, ident, generics) = (item.vis, item.ident, item.generics);
            let predicates = &generics.where_clause;
            quote!(#vis union #ident #generics #predicates)
        }
        "trait" => {
            let item = syn::parse_str::<syn::ItemTrait>(source).ok()?;
            let (vis, unsafety, ident, generics, colon, bounds) = (
                item.vis,
                item.unsafety,
                item.ident,
                item.generics,
                item.colon_token,
                item.supertraits,
            );
            let predicates = &generics.where_clause;
            quote!(#vis #unsafety trait #ident #generics #colon #bounds #predicates)
        }
        "static" => {
            let item = syn::parse_str::<syn::ItemStatic>(source).ok()?;
            let (vis, mutability, ident, ty) = (item.vis, item.mutability, item.ident, item.ty);
            quote!(#vis static #mutability #ident: #ty)
        }
        "constant" | "assoc_const" => {
            if let Ok(item) = syn::parse_str::<syn::ItemConst>(source) {
                let (vis, ident, ty) = (item.vis, item.ident, item.ty);
                quote!(#vis const #ident: #ty)
            } else {
                let item = syn::parse_str::<syn::TraitItemConst>(source).ok()?;
                let (ident, ty) = (item.ident, item.ty);
                quote!(const #ident: #ty)
            }
        }
        "type_alias" | "assoc_type" => {
            if let Ok(item) = syn::parse_str::<syn::ItemType>(source) {
                item.into_token_stream()
            } else {
                syn::parse_str::<syn::TraitItemType>(source)
                    .ok()?
                    .into_token_stream()
            }
        }
        _ => return None,
    };
    Some(tokens.to_string())
}

#[cfg(test)]
mod tests {
    use super::{signature, snippet};
    use serde_json::json;

    #[test]
    fn a_span_uses_character_columns_and_excludes_the_function_body_from_its_signature() {
        let source = "/* é */ pub async fn one<T: Copy>(x: T) -> T {\n    x\n}\n";
        let span = json!({"begin": [1, 9], "end": [3, 2]});
        let declaration = snippet(source, &span).expect("the function span");
        assert_eq!(
            signature(declaration, "function").as_deref(),
            Some("pub async fn one < T : Copy > (x : T) -> T")
        );
    }

    #[test]
    fn a_trait_method_without_a_body_keeps_its_signature() {
        let sig = signature("fn one(&self) -> u32;", "function");
        assert_eq!(sig.as_deref(), Some("fn one (& self) -> u32"));
    }
}
