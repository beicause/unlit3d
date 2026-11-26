use std::{collections::BTreeSet, path::Path};

use serde_json::{Value, json};

use super::Document;
use crate::api_index::source::Sources;

fn item(name: Option<&str>, visibility: &str, inner: Value) -> Value {
    json!({
        "name": name,
        "crate_id": 0,
        "visibility": visibility,
        "inner": inner,
        "span": {"filename": "src/api_index/document/tests.rs", "begin": [1, 1], "end": [1, 2]},
        "docs": "\nFirst line.\nSecond line.",
    })
}

#[test]
fn an_unrecognized_format_is_not_read_even_if_its_other_fields_are_present() {
    let value = json!({"format_version": 62, "root": 0, "index": {}, "paths": {}});
    let Err(error) = Document::parse(&value) else {
        panic!("the schema is unsupported")
    };
    assert!(error.contains("format_version 62 (supported: 61)"));
}

#[test]
fn the_walk_follows_public_aliases_and_globs_but_omits_synthetic_and_blanket_impls() {
    let fixture = json!({
        "format_version": 61, "root": 0, "paths": {"15": {"path": ["fixture", "MyTrait"]}},
        "index": {
            "0": item(Some("fixture"), "public", json!({"module": {"items": [1, 2, 3, 11]}})),
            "1": item(Some("hidden"), "crate", json!({"module": {"is_stripped": true, "items": [4, 12]}})),
            "2": item(None, "public", json!({"use": {"name": "Alias", "id": 4, "is_glob": false}})),
            "3": item(None, "public", json!({"use": {"name": "hidden", "id": 1, "is_glob": true}})),
            "4": item(Some("Thing"), "public", json!({"struct": {"kind": "unit", "impls": [5, 6, 7, 8]}})),
            "5": item(None, "default", json!({"impl": {"trait": null, "items": [9, 10]}})),
            "6": item(None, "default", json!({"impl": {"trait": {"id": 15}, "items": [13]}})),
            "7": item(None, "default", json!({"impl": {"trait": {"id": 15}, "items": [14], "is_synthetic": true}})),
            "8": item(None, "default", json!({"impl": {"trait": {"id": 15}, "items": [14], "blanket_impl": {"generic": "T"}}})),
            "9": item(Some("new"), "public", json!({"function": {}})),
            "10": item(Some("private_method"), "default", json!({"function": {}})),
            "11": item(Some("private_fn"), "default", json!({"function": {}})),
            "12": item(None, "public", json!({"use": {"name": "cycle", "id": 0, "is_glob": true}})),
            "13": item(Some("run"), "default", json!({"function": {}})),
            "14": item(Some("fake"), "public", json!({"function": {}})),
        }
    });
    let doc = Document::parse(&fixture).expect("the supported schema");
    let mut sources = Sources::new(Path::new(env!("CARGO_MANIFEST_DIR")));
    let mut entries = BTreeSet::new();
    doc.collect(&mut sources, false, &mut entries)
        .expect("the crate is indexed");
    let paths: Vec<_> = entries.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "<fixture::Alias as fixture::MyTrait>::run",
            "<fixture::Thing as fixture::MyTrait>::run",
            "fixture",
            "fixture::Alias",
            "fixture::Alias::new",
            "fixture::Thing",
            "fixture::Thing::new",
        ]
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.docs.as_deref() == Some("First line."))
    );

    let mut private = BTreeSet::new();
    doc.collect(&mut sources, true, &mut private)
        .expect("private declarations are indexed");
    for path in [
        "fixture::private_fn",
        "fixture::hidden::Thing::private_method",
    ] {
        assert!(
            private.iter().any(|entry| entry.path == path),
            "missing {path}"
        );
    }
}
