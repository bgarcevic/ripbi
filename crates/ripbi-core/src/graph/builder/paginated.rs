//! Report-local declarations, consumers and query result lineage.
use super::{
    BindingEdge, BindingSite, Builder, ModelIndex, NameKey, ObjectId, Provenance, ReportModel,
    SiteLabels, TabularDatabase,
};
use crate::report::{ReportQuery, paginated::LocalObject};
use crate::{dax, usage};

impl Builder {
    pub(super) fn add_paginated_objects(&mut self, report: &ReportModel) {
        if let Some(rdl) = &report.paginated {
            let name = report.name.as_deref().unwrap_or("");
            let id = |object: &crate::report::paginated::LocalObject| ObjectId::Paginated {
                report: name.into(),
                object: object.clone(),
            };
            for d in &rdl.objects {
                self.node(&id(&d.object));
            }
            let site = |owner: &str, property: &str| {
                Provenance::Binding(Box::new(BindingEdge {
                    kind: BindingSite::Paginated {
                        owner: owner.into(),
                        property: property.into(),
                    },
                    report: Some(NameKey::new(name)),
                    page: None,
                    visual: None,
                    bookmark: None,
                    mobile: false,
                    labels: SiteLabels::default(),
                }))
            };
            for declaration in &rdl.objects {
                if let LocalObject::Field(dataset, _) = &declaration.object {
                    self.edge(
                        &id(&declaration.object),
                        &id(&LocalObject::Dataset(dataset.clone())),
                        site(dataset, "dataset field"),
                    );
                }
            }
            // References inside a declaration depend on its own reachability.
            for r in &rdl.references {
                let target = id(&r.target);
                if self.nodes.contains_key(&target) {
                    if let Some(consumer) = &r.consumer {
                        self.edge(&id(consumer), &target, site(&r.owner, &r.property));
                    } else {
                        self.root(target, site(&r.owner, &r.property));
                    }
                }
            }
            if rdl.incomplete_model {
                let candidates: Vec<_> = self
                    .nodes
                    .keys()
                    .filter(|id| {
                        !matches!(
                            id,
                            ObjectId::Paginated { .. }
                                | ObjectId::ReportMeasure { .. }
                                | ObjectId::Bookmark { .. }
                        )
                    })
                    .cloned()
                    .collect();
                for target in candidates {
                    self.root(target, site(name, "uninspected semantic dataset query"));
                }
            }
            for d in &rdl.objects {
                if d.used {
                    self.root(id(&d.object), site(name, "retained declaration"));
                }
            }
        }
    }

    pub(super) fn add_dataset_lineage(
        &mut self,
        db: &TabularDatabase,
        index: &ModelIndex,
        report: &ReportModel,
        query: &ReportQuery,
        targets: &[ObjectId],
        provenance: &Provenance,
    ) {
        let (Some(rdl), Some(dataset)) = (&report.paginated, query.dataset.as_deref()) else {
            return;
        };
        let id = |object| ObjectId::Paginated {
            report: report.name.clone().unwrap_or_default(),
            object,
        };
        let dataset_id = id(LocalObject::Dataset(dataset.into()));
        for target in targets {
            self.edge(&dataset_id, target, provenance.clone());
        }
        let mdx = matches!(
            usage::sniff_language(&query.text),
            usage::QueryLanguage::Mdx
        );
        let projections = if mdx {
            Vec::new()
        } else {
            row_projections(&query.text)
        };
        for field in &rdl.objects {
            let LocalObject::Field(ds, _) = &field.object else {
                continue;
            };
            if ds != dataset {
                continue;
            }
            let field_id = id(field.object.clone());
            let Some(written) = &field.data_field else {
                continue;
            };
            let candidates = if mdx && written.starts_with('[') && written.contains("].[") {
                usage::mdx_references(db, index, written)
            } else if let Some((_, expr)) = projections.iter().find(|(alias, _)| {
                alias.eq_ignore_ascii_case(written)
                    || format!("[{alias}]").eq_ignore_ascii_case(written)
            }) {
                usage::dax_references(db, index, expr)
            } else {
                continue;
            };
            for target in candidates.iter().filter(|t| targets.contains(t)) {
                self.edge(&field_id, target, provenance.clone());
            }
        }
    }
}

// A top-level ROW or SELECTCOLUMNS explicitly names each output expression.
// For opaque projections the dataset-to-query edges still explain consumption.
fn row_projections(query: &str) -> Vec<(String, &str)> {
    use dax::TokenKind::*;
    let tokens: Vec<_> = dax::tokenize(query)
        .into_iter()
        .filter(|t| !matches!(t.kind, Comment | Eof))
        .collect();
    let [evaluate, function, open, body @ .., close] = tokens.as_slice() else {
        return Vec::new();
    };
    if !evaluate.text.eq_ignore_ascii_case("EVALUATE")
        || open.kind != OpenParen
        || close.kind != CloseParen
    {
        return Vec::new();
    }
    let skip = if function.text.eq_ignore_ascii_case("ROW") {
        0
    } else if function.text.eq_ignore_ascii_case("SELECTCOLUMNS") {
        1
    } else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let args: Vec<_> = body
        .split(|token| {
            match token.kind {
                OpenParen | OpenBrace => depth += 1,
                CloseParen | CloseBrace => depth -= 1,
                _ => {}
            }
            depth == 0 && token.kind == Comma
        })
        .skip(skip)
        .collect();
    args.as_chunks::<2>()
        .0
        .iter()
        .filter_map(|pair| {
            let [alias] = pair[0] else { return None };
            if alias.kind != String {
                return None;
            }
            let expr = pair[1];
            let name = alias
                .text
                .strip_prefix('"')?
                .strip_suffix('"')?
                .replace("\"\"", "\"");
            Some((name, &query[expr.first()?.start..expr.last()?.end()]))
        })
        .collect()
}

// Report Builder expands this macro before the server sees the DAX. Its third
// argument uses MDX-style brackets to identify the model column.
pub(super) fn filter_columns(
    db: &TabularDatabase,
    index: &ModelIndex,
    text: &str,
) -> Vec<ObjectId> {
    use dax::TokenKind::*;
    if !text
        .as_bytes()
        .windows("RSCustomDaxFilter".len())
        .any(|s| s.eq_ignore_ascii_case(b"RSCustomDaxFilter"))
    {
        return Vec::new();
    }
    let tokens: Vec<_> = dax::tokenize(text)
        .into_iter()
        .filter(|t| t.kind != Comment)
        .collect();
    tokens
        .windows(10)
        .filter_map(|t| {
            if !t[0].text.eq_ignore_ascii_case("RSCustomDaxFilter")
                || t[1..10].iter().map(|t| t.kind).ne([
                    OpenParen,
                    QueryParameter,
                    Comma,
                    Identifier,
                    Comma,
                    BracketName,
                    Dot,
                    BracketName,
                    Comma,
                ])
            {
                return None;
            }
            let inner = |n: usize| t[n].text.strip_prefix('[')?.strip_suffix(']');
            let column = index.resolve_column(inner(6)?, inner(8)?)?;
            db.object_id(crate::model::index::Resolved::Column(column))
        })
        .collect()
}
