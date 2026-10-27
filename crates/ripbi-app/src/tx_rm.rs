//! `scan --emit tx-rm`: the unused findings as JSON Lines for tomix's batch
//! removal, `tx rm --from -`. One object per line; `tx` resolves every line
//! before anything changes and judges the whole set at once, so the order of
//! the lines does not matter and a dead table's members may be listed beside
//! it (`tx` reports them as `subsumed`).
//!
//! Each line names its object the way `tx` builds paths — `type`, `table`,
//! `name` — so no path quoting is needed and `type` settles a column and a
//! partition that share a name. `id` carries ripbi's display id; `tx`
//! ignores it but echoes the line back as the target's `input`, so a result
//! traces back to its finding. Report measures and bookmarks live in the
//! report, not the model: they are `#` comment lines, which `tx` skips.
//!
//! The JSON lines are pure ASCII: every non-ASCII character is a `\uXXXX`
//! escape. A shell that re-encodes a pipe (Windows PowerShell decodes native
//! output with its console code page and re-encodes it for the next command)
//! would otherwise mangle a name like `Salg æøå`; ASCII passes through every
//! shell unchanged, and `tx` decodes the escapes back to the same name.

use std::io::{self, Write};

use serde::Serialize;

use ripbi_core::{ObjectId, TabularDatabase};

/// One `tx rm --from` line.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct Target<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    table: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    /// Only for a relationship the model gives no name: its endpoint form.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    id: String,
}

/// Writes one line per finding (the reported unused objects, in their
/// order) of the model at `target`.
///
/// # Errors
/// Propagates stream write failures.
pub fn write(
    out: &mut dyn Write,
    target: &str,
    db: &TabularDatabase,
    findings: &[&ObjectId],
) -> io::Result<()> {
    writeln!(out, "# ripbi unused objects in {target}")?;
    writeln!(
        out,
        "# Remove them as one change: tx rm --from <this-file> <model> --save"
    )?;
    let mut targets = 0;
    for id in findings {
        match tx_target(id, db) {
            Some(line) => {
                let mut json = serde_json::Serializer::with_formatter(&mut *out, AsciiFormatter);
                line.serialize(&mut json)?;
                writeln!(out)?;
                targets += 1;
            }
            None => writeln!(out, "# skipped {id}: lives in the report, not the model")?,
        }
    }
    if targets == 0 {
        writeln!(out, "# Nothing to remove.")?;
    }
    Ok(())
}

/// The `tx` target for a model object; `None` for a report-side object.
fn tx_target<'a>(id: &'a ObjectId, db: &'a TabularDatabase) -> Option<Target<'a>> {
    let line = |kind, table: Option<&'a str>, name: &'a str| Target {
        kind,
        table,
        name: Some(name),
        path: None,
        id: id.to_string(),
    };
    Some(match id {
        ObjectId::Table { table } => line("table", None, table.as_str()),
        ObjectId::Column { table, column } => line("column", Some(table.as_str()), column.as_str()),
        ObjectId::Measure { table, measure } => {
            line("measure", Some(table.as_str()), measure.as_str())
        }
        ObjectId::Hierarchy { table, hierarchy } => {
            line("hierarchy", Some(table.as_str()), hierarchy.as_str())
        }
        ObjectId::Partition { table, partition } => {
            line("partition", Some(table.as_str()), partition.as_str())
        }
        ObjectId::CalculationItem { table, item } => {
            line("calculation_item", Some(table.as_str()), item.as_str())
        }
        ObjectId::Role { role } => line("role", None, role.as_str()),
        ObjectId::Expression { name } => line("expression", None, name.as_str()),
        ObjectId::Function { name } => line("function", None, name.as_str()),
        ObjectId::Relationship {
            from_table,
            from_column,
            to_table,
            to_column,
        } => {
            let named = db.relationships.iter().find(|rel| {
                [
                    (&rel.from_table, from_table),
                    (&rel.from_column, from_column),
                    (&rel.to_table, to_table),
                    (&rel.to_column, to_column),
                ]
                .into_iter()
                .all(|(name, key)| ripbi_core::NameKey::new(name.as_str()) == *key)
            });
            match named
                .and_then(|rel| rel.name.as_deref())
                .filter(|n| !n.is_empty())
            {
                Some(name) => line("relationship", None, name),
                // `tx` resolves the endpoint form itself.
                None => Target {
                    kind: "relationship",
                    table: None,
                    name: None,
                    path: Some(format!(
                        "{}[{}] -> {}[{}]",
                        from_table.quoted(),
                        bracketed(from_column.as_str()),
                        to_table.quoted(),
                        bracketed(to_column.as_str()),
                    )),
                    id: id.to_string(),
                },
            }
        }
        ObjectId::ReportMeasure { .. } | ObjectId::Bookmark { .. } => return None,
    })
}

/// Compact JSON with every non-ASCII character escaped as `\uXXXX` (a UTF-16
/// surrogate pair beyond the BMP), so a line survives any pipe encoding.
struct AsciiFormatter;

impl serde_json::ser::Formatter for AsciiFormatter {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut ascii_from = 0;
        for (at, ch) in fragment.char_indices().filter(|(_, ch)| !ch.is_ascii()) {
            writer.write_all(&fragment.as_bytes()[ascii_from..at])?;
            for unit in ch.encode_utf16(&mut [0; 2]) {
                write!(writer, "\\u{unit:04x}")?;
            }
            ascii_from = at + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[ascii_from..])
    }
}

/// A column name inside DAX brackets: `]` doubles.
fn bracketed(name: &str) -> String {
    name.replace(']', "]]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ripbi_core::{NameKey, Relationship};

    fn render(db: &TabularDatabase, findings: &[ObjectId]) -> String {
        let refs: Vec<&ObjectId> = findings.iter().collect();
        let mut out = Vec::new();
        write(&mut out, "model", db, &refs).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The non-comment lines, parsed.
    fn targets(output: &str) -> Vec<serde_json::Value> {
        output
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn column(table: &str, column: &str) -> ObjectId {
        ObjectId::Column {
            table: NameKey::new(table),
            column: NameKey::new(column),
        }
    }

    #[test]
    fn every_model_kind_names_its_type() {
        let db = TabularDatabase {
            relationships: vec![Relationship {
                name: Some("rel-1".to_string()),
                from_table: "Sales".to_string(),
                from_column: "Key".to_string(),
                to_table: "Dim".to_string(),
                to_column: "Key".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let table = |name: &str| NameKey::new(name);
        let output = render(
            &db,
            &[
                ObjectId::Table {
                    table: table("Staging"),
                },
                column("Sales", "Amount"),
                ObjectId::Measure {
                    table: table("Sales"),
                    measure: table("Total"),
                },
                ObjectId::Hierarchy {
                    table: table("Date"),
                    hierarchy: table("Fiscal"),
                },
                ObjectId::Partition {
                    table: table("Sales"),
                    partition: table("Sales"),
                },
                ObjectId::CalculationItem {
                    table: table("Time Intelligence"),
                    item: table("YTD"),
                },
                ObjectId::Role {
                    role: table("Readers"),
                },
                ObjectId::Expression {
                    name: table("Server"),
                },
                ObjectId::Function {
                    name: table("Double"),
                },
                ObjectId::Relationship {
                    from_table: table("sales"),
                    from_column: table("key"),
                    to_table: table("Dim"),
                    to_column: table("Key"),
                },
            ],
        );
        let shapes: Vec<String> = targets(&output)
            .iter()
            .map(|target| {
                format!(
                    "{} {} {}",
                    target["type"].as_str().unwrap(),
                    target["table"].as_str().unwrap_or("-"),
                    target["name"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            shapes,
            [
                "table - Staging",
                "column Sales Amount",
                "measure Sales Total",
                "hierarchy Date Fiscal",
                "partition Sales Sales",
                "calculation_item Time Intelligence YTD",
                "role - Readers",
                "expression - Server",
                "function - Double",
                "relationship - rel-1",
            ]
        );
    }

    #[test]
    fn names_need_no_quoting_and_every_line_keeps_its_id() {
        let names = ["Net Amount", "O'Neil *", "a/b", "say \"hi\"", "Measures"];
        let findings: Vec<ObjectId> = names.iter().map(|name| column("T", name)).collect();
        let output = render(&TabularDatabase::default(), &findings);
        let targets = targets(&output);
        for (target, (name, id)) in targets.iter().zip(names.iter().zip(&findings)) {
            assert_eq!(target["name"], *name);
            assert_eq!(target["id"], id.to_string());
        }
        assert_eq!(targets.len(), names.len());
    }

    #[test]
    fn non_ascii_names_are_escaped_and_round_trip() {
        let names = ["Salg æøå", "Größe", "売上", "Revenue 📈"];
        let findings: Vec<ObjectId> = names.iter().map(|name| column("Fakta", name)).collect();
        let output = render(&TabularDatabase::default(), &findings);
        let lines: Vec<&str> = output.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(lines.iter().all(|line| line.is_ascii()), "{output}");
        let esc = |hex: &str| format!("{}u{hex}", '\\');
        let expected = format!(
            r#""name":"Salg {}{}{}""#,
            esc("00e6"),
            esc("00f8"),
            esc("00e5")
        );
        assert!(lines[0].contains(&expected), "{output}");
        let chart = format!("{}{}", esc("d83d"), esc("dcc8"));
        assert!(lines[3].contains(&chart), "{output}");
        for (target, name) in targets(&output).iter().zip(names) {
            assert_eq!(target["name"], name);
        }
    }

    #[test]
    fn an_unnamed_relationship_uses_its_endpoints() {
        let output = render(
            &TabularDatabase::default(),
            &[ObjectId::Relationship {
                from_table: NameKey::new("O'Brien"),
                from_column: NameKey::new("x]"),
                to_table: NameKey::new("B"),
                to_column: NameKey::new("y"),
            }],
        );
        let target = &targets(&output)[0];
        assert_eq!(target["type"], "relationship");
        assert_eq!(target["path"], "'O''Brien'[x]]] -> 'B'[y]");
        assert!(target.get("name").is_none());
    }

    #[test]
    fn report_objects_are_comments_and_an_empty_list_says_so() {
        let output = render(
            &TabularDatabase::default(),
            &[ObjectId::ReportMeasure {
                measure: NameKey::new("Local"),
            }],
        );
        assert!(targets(&output).is_empty());
        assert!(
            output.contains("# skipped report measure 'Local': lives in the report"),
            "{output}"
        );
        assert!(output.ends_with("# Nothing to remove.\n"), "{output}");
    }
}
