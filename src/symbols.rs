use serde_json::Value;

/// アウトライン表示用に平坦化したシンボル1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolEntry {
    pub name: String,
    /// SymbolKind の英語名。不明な番号は "Unknown"。
    pub kind: String,
    /// 入れ子の深さ（トップレベル = 0）。SymbolInformation 形式では常に 0。
    pub depth: usize,
    /// 0始まりの行（ジャンプ先）。
    pub line: usize,
    /// 0始まりの文字位置（ジャンプ先）。
    pub character: usize,
    /// SymbolInformation 形式の containerName（あれば）。DocumentSymbol 形式では None。
    pub container: Option<String>,
}

/// textDocument/documentSymbol の result（JSON）を平坦な一覧にする。
/// DocumentSymbol は selectionRange（なければ range）へジャンプし、
/// children を深さ優先で展開する。SymbolInformation は location.range を使う。
/// 配列以外は空の一覧を返し、壊れた要素は飛ばす。
pub fn parse_document_symbols(result: &serde_json::Value) -> Vec<SymbolEntry> {
    let Some(items) = result.as_array() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    // 明示的なスタックにより、深い children でも再帰呼び出しを避ける。
    let mut pending: Vec<_> = items.iter().rev().map(|item| (item, 0)).collect();
    while let Some((item, depth)) = pending.pop() {
        if let Some(entry) = parse_entry(item, depth) {
            entries.push(entry);
        }
        if item.get("location").is_none() {
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                pending.extend(children.iter().rev().map(|child| (child, depth + 1)));
            }
        }
    }
    entries
}

fn parse_entry(item: &Value, depth: usize) -> Option<SymbolEntry> {
    let name = item.get("name")?.as_str()?;
    let kind = item.get("kind")?;
    if !kind.is_i64() && !kind.is_u64() {
        return None;
    }
    let (position, depth, container) = if let Some(location) = item.get("location") {
        (
            range_start(location.get("range")?)?,
            0,
            item.get("containerName")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
    } else {
        (
            item.get("selectionRange")
                .and_then(range_start)
                .or_else(|| item.get("range").and_then(range_start))?,
            depth,
            None,
        )
    };
    Some(SymbolEntry {
        name: name.to_owned(),
        kind: kind_name(kind.as_u64()).to_owned(),
        depth,
        line: position.0,
        character: position.1,
        container,
    })
}

fn range_start(range: &Value) -> Option<(usize, usize)> {
    let start = range.get("start")?;
    Some((
        usize::try_from(start.get("line")?.as_u64()?).ok()?,
        usize::try_from(start.get("character")?.as_u64()?).ok()?,
    ))
}

fn kind_name(kind: Option<u64>) -> &'static str {
    match kind {
        Some(1) => "File",
        Some(2) => "Module",
        Some(3) => "Namespace",
        Some(4) => "Package",
        Some(5) => "Class",
        Some(6) => "Method",
        Some(7) => "Property",
        Some(8) => "Field",
        Some(9) => "Constructor",
        Some(10) => "Enum",
        Some(11) => "Interface",
        Some(12) => "Function",
        Some(13) => "Variable",
        Some(14) => "Constant",
        Some(15) => "String",
        Some(16) => "Number",
        Some(17) => "Boolean",
        Some(18) => "Array",
        Some(19) => "Object",
        Some(20) => "Key",
        Some(21) => "Null",
        Some(22) => "EnumMember",
        Some(23) => "Struct",
        Some(24) => "Event",
        Some(25) => "Operator",
        Some(26) => "TypeParameter",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn symbol(name: &str, kind: i64) -> Value {
        json!({"name": name, "kind": kind,
            "range": {"start": {"line": 0, "character": 0}}})
    }

    #[test]
    fn document_symbols_are_depth_first_and_prefer_selection() {
        let mut parent = symbol("Widget", 23);
        parent["selectionRange"] = json!({"start": {"line": 2, "character": 7}});
        parent["containerName"] = json!("ignored");
        let mut method = symbol("run", 6);
        method["selectionRange"] = json!({"start": {"line": 4, "character": 9}});
        parent["children"] = json!([method]);
        let entries = parse_document_symbols(&json!([parent, symbol("next", 12)]));
        assert_eq!(
            entries,
            vec![
                SymbolEntry {
                    name: "Widget".into(),
                    kind: "Struct".into(),
                    depth: 0,
                    line: 2,
                    character: 7,
                    container: None
                },
                SymbolEntry {
                    name: "run".into(),
                    kind: "Method".into(),
                    depth: 1,
                    line: 4,
                    character: 9,
                    container: None
                },
                SymbolEntry {
                    name: "next".into(),
                    kind: "Function".into(),
                    depth: 0,
                    line: 0,
                    character: 0,
                    container: None
                },
            ]
        );
    }

    #[test]
    fn symbol_information_uses_location_and_container() {
        let entries = parse_document_symbols(&json!([
            {"name": "run", "kind": 12, "containerName": "Widget",
             "location": {"uri": "file:///example.rs",
                 "range": {"start": {"line": 8, "character": 3}}}},
            {"name": "value", "kind": 13,
             "location": {"range": {"start": {"line": 0, "character": 0}}}}
        ]));
        assert_eq!(
            entries[0],
            SymbolEntry {
                name: "run".into(),
                kind: "Function".into(),
                depth: 0,
                line: 8,
                character: 3,
                container: Some("Widget".into())
            }
        );
        assert_eq!(entries[1].depth, 0);
        assert_eq!(entries[1].container, None);
    }

    #[test]
    fn all_kinds_and_unknown_numbers() {
        let expected = [
            "File",
            "Module",
            "Namespace",
            "Package",
            "Class",
            "Method",
            "Property",
            "Field",
            "Constructor",
            "Enum",
            "Interface",
            "Function",
            "Variable",
            "Constant",
            "String",
            "Number",
            "Boolean",
            "Array",
            "Object",
            "Key",
            "Null",
            "EnumMember",
            "Struct",
            "Event",
            "Operator",
            "TypeParameter",
        ];
        for (index, expected) in expected.iter().enumerate() {
            let entries = parse_document_symbols(&json!([symbol("x", index as i64 + 1)]));
            assert_eq!(&entries[0].kind, expected);
        }
        for kind in [-1, 0, 27, i64::MAX] {
            assert_eq!(
                parse_document_symbols(&json!([symbol("x", kind)]))[0].kind,
                "Unknown"
            );
        }
    }

    #[test]
    fn empty_and_non_arrays_are_empty() {
        for input in [json!([]), Value::Null, json!({}), json!("text"), json!(42)] {
            assert!(parse_document_symbols(&input).is_empty());
        }
    }

    #[test]
    fn malformed_entries_are_skipped_and_valid_siblings_survive() {
        let entries = parse_document_symbols(&json!([
            null, 7, {}, {"name": 5, "kind": 12},
            {"name": "bad", "kind": "12"},
            {"name": "bad", "kind": 12, "range": {"start": {"line": -1, "character": 0}}},
            {"name": "bad", "kind": 12, "location": null},
            {"name": "bad", "kind": 12, "range": {"start": {"line": 0}}},
            symbol("valid", 12)
        ]));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "valid");
    }

    #[test]
    fn malformed_selection_falls_back_to_range_and_children_survive() {
        let mut parent = symbol("parent", 23);
        parent["selectionRange"] = json!({"start": {"line": "bad", "character": 0}});
        parent["children"] = json!([null, symbol("child", 6)]);
        let entries = parse_document_symbols(&json!([parent]));
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].line, entries[0].character), (0, 0));
        assert_eq!(entries[1].depth, 1);
    }
}
