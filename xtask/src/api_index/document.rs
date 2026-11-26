//! Walk documented module exports and real associated declarations.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{FORMAT_VERSION, source::Sources};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Entry {
    path: String,
    kind: String,
    file: String,
    line: u64,
    signature: Option<String>,
    docs: Option<String>,
    visibility: String,
    deprecated: Option<(Option<String>, Option<String>)>,
}

impl Entry {
    pub(super) fn to_value(&self) -> Value {
        json!({
            "path": self.path,
            "kind": self.kind,
            "file": self.file,
            "line": self.line,
            "signature": self.signature,
            "doc": self.docs,
            "visibility": self.visibility,
            "deprecated": self.deprecated.as_ref().map(|(since, note)| json!({"since": since, "note": note})),
        })
    }
}

pub(super) struct Document<'a> {
    root: u64,
    index: &'a Map<String, Value>,
    paths: &'a Map<String, Value>,
}

impl<'a> Document<'a> {
    pub(super) fn parse(doc: &'a Value) -> Result<Self, String> {
        if doc["format_version"].as_u64() != Some(FORMAT_VERSION) {
            return Err(format!(
                "unsupported rustdoc JSON format_version {} (supported: {FORMAT_VERSION}); update the API index parser and its fixtures for this schema, or use a compatible nightly",
                doc["format_version"]
            ));
        }
        let root = doc["root"]
            .as_u64()
            .ok_or("rustdoc JSON is missing its root ID")?;
        let index = doc["index"]
            .as_object()
            .ok_or("rustdoc JSON is missing its item index")?;
        let paths = doc["paths"]
            .as_object()
            .ok_or("rustdoc JSON is missing its path index")?;
        Ok(Self { root, index, paths })
    }

    pub(super) fn collect(
        &self,
        sources: &mut Sources<'_>,
        private: bool,
        entries: &mut BTreeSet<Entry>,
    ) -> Result<(), String> {
        let root = self
            .item(self.root)
            .ok_or("rustdoc JSON root is not in its item index")?;
        let name = root["name"]
            .as_str()
            .ok_or("rustdoc JSON root has no crate name")?;
        let mut walk = Walk {
            doc: self,
            private,
            sources,
            entries,
            modules: Vec::new(),
        };
        walk.visit(self.root, root, name.to_owned(), false);
        Ok(())
    }

    fn item(&self, id: u64) -> Option<&'a Value> {
        self.index.get(&id.to_string())
    }
}

struct Walk<'a, 's, 'root> {
    doc: &'a Document<'a>,
    private: bool,
    sources: &'s mut Sources<'root>,
    entries: &'s mut BTreeSet<Entry>,
    // A re-export may lead back to an ancestor. Aliases on independent paths
    // still get their own walk and associated entries.
    modules: Vec<u64>,
}

impl Walk<'_, '_, '_> {
    fn visit(&mut self, id: u64, item: &Value, path: String, implicit: bool) {
        let Some((kind, data)) = item_kind(item) else {
            return;
        };
        if kind != "impl" && (kind != "module" || data["is_stripped"] != true) {
            self.record(item, path.clone(), kind, implicit);
        }
        match kind {
            "module" => self.module(id, data, &path, false),
            "struct" => {
                let fields = data["kind"]
                    .get("plain")
                    .map_or(&data["kind"]["tuple"], |plain| &plain["fields"]);
                self.members(fields, &path, false);
                self.implementations(&data["impls"], &path);
            }
            "union" => {
                self.members(&data["fields"], &path, false);
                self.implementations(&data["impls"], &path);
            }
            "enum" => {
                self.members(&data["variants"], &path, true);
                self.implementations(&data["impls"], &path);
            }
            "variant" => {
                let fields = data["kind"]
                    .get("struct")
                    .map_or(&data["kind"]["tuple"], |structure| &structure["fields"]);
                self.members(fields, &path, true);
            }
            "trait" => self.members(&data["items"], &path, true),
            "primitive" => self.implementations(&data["impls"], &path),
            _ => {}
        }
    }

    fn module(&mut self, id: u64, module: &Value, prefix: &str, glob: bool) {
        if self.modules.contains(&id) || (!glob && module["is_stripped"] == true && !self.private) {
            return;
        }
        self.modules.push(id);
        for id in ids(&module["items"]) {
            let Some(item) = self.doc.item(id) else {
                continue;
            };
            if item["visibility"] != "public" && (!self.private || glob) {
                continue;
            }
            if let Some(import) = item["inner"].get("use") {
                self.export(item, import, prefix);
            } else if let Some(name) = item["name"].as_str() {
                self.visit(id, item, format!("{prefix}::{name}"), false);
            }
        }
        self.modules.pop();
    }

    fn export(&mut self, item: &Value, import: &Value, prefix: &str) {
        let target = import["id"]
            .as_u64()
            .and_then(|id| self.doc.item(id).map(|item| (id, item)));
        if import["is_glob"] == true {
            if let Some((id, target)) = target
                && let Some(module) = target["inner"].get("module")
            {
                self.module(id, module, prefix, true);
            }
            return;
        }
        let Some(name) = import["name"].as_str() else {
            return;
        };
        let path = format!("{prefix}::{name}");
        if let Some((id, target)) = target.filter(|(_, target)| target["crate_id"] == 0) {
            self.visit(id, target, path, item["visibility"] == "public");
        } else {
            self.record(item, path, "use", false);
        }
    }

    fn members(&mut self, items: &Value, prefix: &str, implicit: bool) {
        for id in ids(items) {
            let Some(item) = self.doc.item(id) else {
                continue;
            };
            if (implicit || self.private || item["visibility"] == "public")
                && let Some(name) = item["name"].as_str()
            {
                self.visit(id, item, format!("{prefix}::{name}"), implicit);
            }
        }
    }

    fn implementations(&mut self, implementations: &Value, prefix: &str) {
        for id in ids(implementations) {
            let Some(item) = self.doc.item(id) else {
                continue;
            };
            let Some(data) = item["inner"].get("impl") else {
                continue;
            };
            if data["is_synthetic"] == true || !data["blanket_impl"].is_null() {
                continue;
            }
            if data["trait"].is_null() {
                self.members(&data["items"], prefix, false);
            } else if let Some(trait_path) = self.trait_path(&data["trait"]) {
                self.members(&data["items"], &format!("<{prefix} as {trait_path}>"), true);
            }
        }
    }

    fn trait_path(&self, trait_ref: &Value) -> Option<String> {
        if let Some(id) = trait_ref["id"].as_u64()
            && let Some(path) = self.doc.paths.get(&id.to_string())
            && let Some(parts) = path["path"].as_array()
        {
            return Some(
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("::"),
            );
        }
        trait_ref["path"].as_str().map(str::to_owned)
    }

    fn record(&mut self, item: &Value, path: String, kind: &str, implicit: bool) {
        let Some((file, line, signature)) = self.sources.declaration(&item["span"], kind) else {
            return;
        };
        let visibility = if implicit || item["visibility"] == "public" {
            "public".to_owned()
        } else if let Some(visibility) = item["visibility"].as_str() {
            visibility.to_owned()
        } else {
            item["visibility"].to_string()
        };
        let docs = item["docs"]
            .as_str()
            .and_then(|docs| docs.lines().map(str::trim).find(|line| !line.is_empty()));
        let deprecated = item["deprecation"].as_object().map(|_| {
            let field = |name| item["deprecation"][name].as_str().map(str::to_owned);
            (field("since"), field("note"))
        });
        self.entries.insert(Entry {
            path,
            kind: kind.to_owned(),
            file,
            line,
            signature,
            docs: docs.map(str::to_owned),
            visibility,
            deprecated,
        });
    }
}

fn item_kind(item: &Value) -> Option<(&str, &Value)> {
    item["inner"]
        .as_object()?
        .iter()
        .next()
        .map(|(kind, data)| (kind.as_str(), data))
}

fn ids(value: &Value) -> impl Iterator<Item = u64> + '_ {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
}

#[cfg(test)]
mod tests;
