//! Read only model schema and report definition members from PBIX/PBIT ZIPs.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use zip::ZipArchive;
use zip::result::ZipError;

use crate::identity::NameKey;
use crate::ingest::{Ingested, legacy, pbir, tmsl};
use crate::m::{self, TokenKind};
use crate::model::{PartitionSource, SharedExpression, TabularDatabase};
use crate::report::ReportModel;
use crate::{Error, Result};

pub(super) fn has_member(path: &Path, member: &str) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return false;
    };
    zip.by_name(member).is_ok()
}

pub(super) fn model(path: &Path) -> Result<Ingested<TabularDatabase>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let bytes = read_member(&mut zip, "DataModelSchema")?.ok_or_else(|| {
        Error::UnsupportedFormat(format!(
            "{} has no DataModelSchema; compressed DataModel decoding is tracked by issue #10",
            path.display()
        ))
    })?;
    let mut skips = Vec::new();
    let mut value = tmsl::load(&bytes, path, &mut skips)?;
    fill_missing_m(&mut value, &mut zip, path)?;
    ensure_m_coverage(&value, path)?;
    Ok(Ingested { value, skips })
}

pub(super) fn report(path: &Path) -> Result<Ingested<ReportModel>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let mut skips = Vec::new();
    let value = if let Some(layout) = read_member(&mut zip, "Report/Layout")? {
        legacy::load(&layout, path, &mut skips)?
    } else if zip.by_name("Report/definition/report.json").is_ok() {
        let mut files = HashMap::new();
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let name = entry.name().to_string();
            if !name.starts_with("Report/definition/")
                && !name.starts_with("Report/definition.mobile/")
                && name != "Report/definition.pbir"
            {
                continue;
            }
            if entry.is_dir() || !name.ends_with(".json") && name != "Report/definition.pbir" {
                continue;
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            files.insert(path.join(name), bytes);
        }
        let source = pbir::Source::Archive(&files);
        let definition = path.join("Report/definition");
        let mut report =
            pbir::load_report_from_source(&definition, stem_name(path), &mut skips, &source)?;
        let mobile = path.join("Report/definition.mobile");
        if source_has_folder(&files, &mobile.join("pages")) {
            report.mobile_pages =
                pbir::load_mobile_pages_from_source(&mobile, &mut skips, &source)?;
        }
        report
    } else {
        return Err(Error::UnsupportedFormat(format!(
            "{} has no Report/Layout or Report/definition/report.json",
            path.display()
        )));
    };
    Ok(Ingested { value, skips })
}

fn source_has_folder(files: &HashMap<PathBuf, Vec<u8>>, folder: &Path) -> bool {
    files.keys().any(|path| path.starts_with(folder))
}

fn stem_name(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
}

fn read_member<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
) -> Result<Option<Vec<u8>>> {
    let mut entry = match zip.by_name(name) {
        Ok(entry) => entry,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn fill_missing_m(
    model: &mut TabularDatabase,
    zip: &mut ZipArchive<File>,
    path: &Path,
) -> Result<()> {
    let missing_partition = model.tables.iter().any(|table| table.partitions.iter().any(|partition| {
        matches!(&partition.source, PartitionSource::M { expression } if expression.trim().is_empty())
    }));
    let missing_shared = model
        .expressions
        .iter()
        .any(|expression| expression.expression.trim().is_empty());
    let Some(bytes) = read_member(zip, "DataMashup")? else {
        if missing_partition || missing_shared {
            return Err(missing_m_error(path));
        }
        return Ok(());
    };
    let queries = mashup_queries(&bytes)?;
    for table in &mut model.tables {
        for partition in &mut table.partitions {
            if let PartitionSource::M { expression } = &mut partition.source
                && expression.trim().is_empty()
            {
                let text = query_named(&queries, &partition.name)
                    .or_else(|| query_named(&queries, &table.name));
                *expression = text.cloned().ok_or_else(|| missing_m_error(path))?;
            }
        }
    }
    for expression in &mut model.expressions {
        if expression.expression.trim().is_empty() {
            expression.expression = query_named(&queries, &expression.name)
                .cloned()
                .ok_or_else(|| missing_m_error(path))?;
        }
    }
    let partition_queries: std::collections::HashSet<NameKey> = model
        .tables
        .iter()
        .flat_map(|table| {
            table
                .partitions
                .iter()
                .filter(|partition| matches!(partition.source, PartitionSource::M { .. }))
                .map(|partition| NameKey::new(&partition.name))
                .chain(std::iter::once(NameKey::new(&table.name)))
        })
        .collect();
    let mut queries: Vec<_> = queries.into_iter().collect();
    queries.sort_by(|left, right| NameKey::new(&left.0).cmp(&NameKey::new(&right.0)));
    for (name, expression) in queries {
        if !partition_queries.contains(&NameKey::new(&name))
            && !model
                .expressions
                .iter()
                .any(|item| NameKey::new(&item.name) == NameKey::new(&name))
        {
            model.expressions.push(SharedExpression {
                name,
                expression,
                ..Default::default()
            });
        }
    }
    Ok(())
}

fn query_named<'a>(queries: &'a HashMap<String, String>, name: &str) -> Option<&'a String> {
    let name = NameKey::new(name);
    queries
        .iter()
        .find_map(|(key, value)| (NameKey::new(key) == name).then_some(value))
}

fn missing_m_error(path: &Path) -> Error {
    Error::UnsupportedFormat(format!(
        "{} has incomplete M expression coverage",
        path.display()
    ))
}

pub(super) fn ensure_m_coverage(model: &TabularDatabase, path: &Path) -> Result<()> {
    if model.tables.iter().any(|table| table.partitions.iter().any(|partition| {
        matches!(&partition.source, PartitionSource::M { expression } if expression.trim().is_empty())
    })) || model.expressions.iter().any(|expression| expression.expression.trim().is_empty()) {
        Err(missing_m_error(path))
    } else {
        Ok(())
    }
}

fn mashup_queries(bytes: &[u8]) -> Result<HashMap<String, String>> {
    // MS-QDEFF stores a length-prefixed PackageParts ZIP after its version.
    let package = bytes
        .get(8..)
        .and_then(|rest| {
            let size = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
            rest.get(..size)
        })
        .ok_or_else(|| Error::UnsupportedFormat("DataMashup has no PackageParts".to_string()))?;
    let mut zip = ZipArchive::new(Cursor::new(package))?;
    let mut queries = HashMap::new();
    let mut sections = 0;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        if !entry.name().starts_with("Formulas/") || !entry.name().ends_with(".m") || entry.is_dir()
        {
            continue;
        }
        sections += 1;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::UnsupportedFormat("DataMashup section is not UTF-8".to_string()))?;
        queries.extend(split_section_queries(
            text.strip_prefix('\u{feff}').unwrap_or(&text),
        ));
    }
    if sections == 0 {
        return Err(Error::UnsupportedFormat(
            "DataMashup has no Formulas section".to_string(),
        ));
    }
    Ok(queries)
}

fn split_section_queries(text: &str) -> HashMap<String, String> {
    let mut queries = HashMap::new();
    let tokens = m::tokenize(text);
    let mut index = 0;
    while index + 3 < tokens.len() {
        if tokens[index].kind != TokenKind::Identifier || tokens[index].text != "shared" {
            index += 1;
            continue;
        }
        let name_token = tokens[index + 1];
        let name = match name_token.kind {
            TokenKind::Identifier => name_token.text.to_string(),
            TokenKind::QuotedIdentifier => name_token
                .text
                .strip_prefix("#\"")
                .and_then(|text| text.strip_suffix('"'))
                .map(|text| text.replace("\"\"", "\""))
                .unwrap_or_default(),
            _ => {
                index += 1;
                continue;
            }
        };
        if name.is_empty() || tokens[index + 2].text != "=" {
            index += 1;
            continue;
        }
        let start = tokens[index + 2].end();
        let mut depth = 0usize;
        let mut end = None;
        for (next, token) in tokens.iter().enumerate().skip(index + 3) {
            match token.kind {
                TokenKind::OpenParen | TokenKind::OpenBracket | TokenKind::OpenBrace => depth += 1,
                TokenKind::CloseParen | TokenKind::CloseBracket | TokenKind::CloseBrace => {
                    depth = depth.saturating_sub(1)
                }
                TokenKind::Semicolon if depth == 0 => {
                    end = Some((next, token.start));
                    break;
                }
                _ => {}
            }
        }
        if let Some((next, end)) = end {
            queries.insert(name, text[start..end].trim().to_string());
            index = next + 1;
        } else {
            break;
        }
    }
    queries
}

#[cfg(test)]
mod tests {
    use super::split_section_queries;

    #[test]
    fn mashup_sections_ignore_shared_inside_strings_and_comments() {
        let source = "section Section1;\nshared #\"Sales Data\" = let x = \"shared Fake = 0;\" in x;\n// shared Phantom = 0;\nshared Parameter = 42;";
        let queries = split_section_queries(source);
        assert_eq!(queries.len(), 2);
        assert_eq!(queries["Sales Data"], "let x = \"shared Fake = 0;\" in x");
        assert_eq!(queries["Parameter"], "42");
    }
}
