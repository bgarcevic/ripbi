//! Static analysis of report definitions. Collect declarations before inspecting
//! expressions because RDL permits references to later siblings.
use super::{
    Ingested, SkipKind, SkipNotice,
    xml::{Element, parse_xml},
};
use crate::report::{ReportModel, paginated::*};
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

fn content<'a>(e: &'a Element, tag: &str) -> &'a str {
    e.child(tag).map_or("", |c| c.text.trim())
}

pub(super) fn load(path: &Path) -> Result<Ingested<ReportModel>> {
    let text = super::decode_json_text(&std::fs::read(path)?, "RDL")?;
    let mut root = parse_xml(text.as_bytes()).map_err(Error::UnsupportedFormat)?;
    if root.name != "Report" || root.foreign_namespace {
        return Err(Error::UnsupportedFormat(
            "expected an RDL Report element".into(),
        ));
    }
    fn remove_extensions(node: &mut Element) {
        node.children.retain(|child| !child.foreign_namespace);
        for child in &mut node.children {
            remove_extensions(child);
        }
    }
    remove_extensions(&mut root);
    let mut state = Analysis {
        path,
        data: PaginatedReport::default(),
        skips: vec![],
        scopes: HashMap::new(),
        unknown: HashSet::new(),
        unknown_all: false,
        consumer: None,
    };
    for (group, tag) in [
        ("DataSources", "DataSource"),
        ("DataSets", "DataSet"),
        ("ReportParameters", "ReportParameter"),
    ] {
        for item in root
            .child(group)
            .into_iter()
            .flat_map(|e| &e.children)
            .filter(|e| e.name == tag)
        {
            let Some(name) = item.attr("Name") else {
                state.opaque(item, None, "unnamed declaration");
                continue;
            };
            match tag {
                "DataSource" => {
                    let props = item.child("ConnectionProperties");
                    let provider = props.map_or("", |p| content(p, "DataProvider"));
                    let connect = props.map_or("", |p| content(p, "ConnectString"));
                    let property = |key: &str| {
                        connect
                            .split(';')
                            .filter_map(|v| v.split_once('='))
                            .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
                            .map(|(_, v)| v.trim().trim_matches(['\'', '"']))
                            .filter(|v| !v.is_empty())
                            .map(str::to_owned)
                    };
                    let catalog = property("Initial Catalog");
                    let model_id = property("semanticmodelid").or_else(|| property("datasetid"));
                    let shared = item.child("DataSourceReference").is_some();
                    state.data.sources.push(Source {
                        shared,
                        name: name.into(),
                        semantic: ["PBIDATASET", "OLEDB-MD", "ANALYSIS SERVICES", "MSOLAP"]
                            .iter()
                            .any(|p| provider.eq_ignore_ascii_case(p)),
                        model_id,
                        catalog,
                    });
                    state.declare(LocalObject::Source(name.into()), item);
                    if item.child("DataSourceReference").is_some() {
                        state.notice(item, "shared data source definition unavailable");
                    }
                }
                "DataSet" => {
                    state.declare(LocalObject::Dataset(name.into()), item);
                    if item.child("SharedDataSet").is_some() {
                        state.opaque(item, Some(name), "shared dataset definition unavailable");
                        state.data.queries.push(DatasetQuery {
                            dataset: name.into(),
                            source: String::new(),
                            line: item.line,
                            text: String::new(),
                        });
                    }
                    if let Some(query) = item.child("Query") {
                        state.data.queries.push(DatasetQuery {
                            dataset: name.into(),
                            source: content(query, "DataSourceName").into(),
                            text: content(query, "CommandText").into(),
                            line: query.line,
                        });
                    }
                    for field in item
                        .child("Fields")
                        .into_iter()
                        .flat_map(|e| &e.children)
                        .filter(|e| e.name == "Field")
                    {
                        if let Some(f) = field.attr("Name") {
                            state.declare(LocalObject::Field(name.into(), f.into()), field);
                        }
                    }
                }
                _ => state.declare(LocalObject::Parameter(name.into()), item),
            }
        }
    }
    let mut datasets = state.data.objects.iter().filter_map(|d| match &d.object {
        LocalObject::Dataset(s) => Some(s.clone()),
        _ => None,
    });
    let default = datasets.next().filter(|_| datasets.next().is_none());
    for d in &state.data.objects {
        if let LocalObject::Dataset(name) = &d.object {
            state.scopes.insert(name.clone(), name.clone());
        }
    }
    collect_scopes(&root, default.as_deref(), &mut state.scopes);
    state.visit(&root, default.as_deref(), "Report", false);
    let declared: HashSet<_> = state
        .data
        .objects
        .iter()
        .map(|d| d.object.clone())
        .collect();
    for d in &mut state.data.objects {
        d.used = state.unknown_all
            || match &d.object {
                LocalObject::Field(dataset, _) | LocalObject::Dataset(dataset) => {
                    state.unknown.contains(dataset)
                }
                LocalObject::Parameter(_) => true, // external callers can supply parameters
                LocalObject::Source(_) => false,
            };
    }
    for r in &mut state.data.references {
        r.broken_filter &= !declared.contains(&r.target)
            && !state.unknown_all
            && !matches!(&r.target,LocalObject::Field(d,_) if state.unknown.contains(d) || !declared.contains(&LocalObject::Dataset(d.clone())));
    }
    Ok(Ingested {
        value: ReportModel {
            name: path.file_name().map(|s| s.to_string_lossy().into_owned()),
            paginated: Some(state.data),
            ..Default::default()
        },
        skips: state.skips,
    })
}

fn data_region(tag: &str) -> bool {
    matches!(
        tag,
        "Tablix" | "List" | "Table" | "Matrix" | "Chart" | "GaugePanel" | "Map"
    )
}

fn collect_scopes(e: &Element, scope: Option<&str>, scopes: &mut HashMap<String, String>) {
    let scope = e.child("DataSetName").map(|n| n.text.trim()).or(scope);
    if (data_region(&e.name) || matches!(e.name.as_str(), "Group" | "Grouping"))
        && let (Some(owner), Some(dataset)) = (e.attr("Name"), scope)
    {
        scopes.insert(owner.into(), dataset.into());
    }
    for child in &e.children {
        collect_scopes(child, scope, scopes);
    }
}

struct Analysis<'a> {
    scopes: HashMap<String, String>,
    path: &'a Path,
    data: PaginatedReport,
    skips: Vec<SkipNotice>,
    unknown: HashSet<String>,
    unknown_all: bool,
    consumer: Option<LocalObject>,
}

impl Analysis<'_> {
    fn declare(&mut self, object: LocalObject, e: &Element) {
        self.data.objects.push(Declaration {
            object,
            line: e.line,
            used: false,
            data_field: e.child("DataField").map(|f| f.text.trim().into()),
        });
    }
    fn notice(&mut self, e: &Element, detail: &str) {
        self.skips.push(SkipNotice {
            path: self.path.into(),
            location: Some(format!("line {}", e.line)),
            kind: SkipKind::NotAnalyzed,
            detail: detail.into(),
        });
    }
    fn opaque(&mut self, e: &Element, scope: Option<&str>, reason: &str) {
        if let Some(s) = scope {
            self.unknown.insert(s.into());
        } else {
            self.unknown_all = true;
        }
        self.notice(e, &format!("{reason} in {}", scope.unwrap_or("report")));
    }
    fn reference(&mut self, target: LocalObject, owner: &str, e: &Element, filter: bool) {
        self.data.references.push(Reference {
            target,
            owner: owner.into(),
            consumer: self.consumer.clone(),
            property: e.name.clone(),
            line: e.line,
            broken_filter: filter,
        });
    }
    fn visit(&mut self, e: &Element, scope: Option<&str>, owner: &str, filter: bool) {
        if matches!(
            e.name.as_str(),
            "DesignerState" | "ConnectString" | "EmbeddedImage"
        ) {
            return;
        }
        let owner = e.attr("Name").unwrap_or(owner);
        let scope = if e.name == "DataSet" {
            e.attr("Name")
        } else {
            e.child("DataSetName").map(|n| n.text.trim()).or(scope)
        };
        let previous = self.consumer.clone();
        match e.name.as_str() {
            "DataSet" => self.consumer = Some(LocalObject::Dataset(owner.into())),
            "Field" => self.consumer = scope.map(|s| LocalObject::Field(s.into(), owner.into())),
            "ReportParameter" => self.consumer = Some(LocalObject::Parameter(owner.into())),
            _ => {}
        }
        let filter = filter || e.name == "Filter";
        if data_region(&e.name)
            && let Some(s) = scope
        {
            self.reference(LocalObject::Dataset(s.into()), owner, e, false);
        }
        let value = e.text.trim();
        match e.name.as_str() {
            "Code" | "CodeModule" if !value.is_empty() => {
                self.opaque(e, None, "custom report code")
            }
            "DataSetName" if !value.is_empty() => {
                self.reference(LocalObject::Dataset(value.into()), owner, e, false)
            }
            "DataSourceName" if !value.is_empty() => {
                self.reference(LocalObject::Source(value.into()), owner, e, false)
            }
            "ValueField" | "LabelField" if !value.is_empty() => {
                self.field(scope, value, owner, e, false)
            }
            _ if value.starts_with('=') => {
                let tokens = tokens(value);
                self.expression(&tokens, scope, owner, e, filter);
            }
            _ => {}
        }
        for child in &e.children {
            self.visit(child, scope, owner, filter);
        }
        self.consumer = previous;
    }
    fn field(&mut self, scope: Option<&str>, name: &str, owner: &str, e: &Element, filter: bool) {
        if let Some(s) = scope {
            self.reference(LocalObject::Field(s.into(), name.into()), owner, e, filter);
            self.reference(LocalObject::Dataset(s.into()), owner, e, false);
        } else {
            self.opaque(e, None, "ambiguous field scope");
        }
    }
    fn expression(
        &mut self,
        t: &[Token<'_>],
        scope: Option<&str>,
        owner: &str,
        e: &Element,
        filter: bool,
    ) {
        let mut i = 0;
        while i < t.len() {
            let Some(Token::Word(word)) = t.get(i) else {
                i += 1;
                continue;
            };
            let fields = word.eq_ignore_ascii_case("Fields");
            if fields || word.eq_ignore_ascii_case("Parameters") {
                let tail = &t[i + 1..];
                let name = match tail {
                    [Token::Punct('!'), Token::Word(name), ..]
                    | [
                        Token::Punct('('),
                        Token::Literal(name),
                        Token::Punct(')'),
                        ..,
                    ]
                    | [
                        Token::Punct('.'),
                        Token::Word("Item"),
                        Token::Punct('('),
                        Token::Literal(name),
                        Token::Punct(')'),
                        ..,
                    ] => Some(*name),
                    _ => None,
                };
                if let Some(name) = name {
                    if fields {
                        self.field(scope, name, owner, e, filter);
                    } else {
                        self.reference(LocalObject::Parameter(name.into()), owner, e, filter);
                    }
                } else {
                    self.opaque(e, scope, "dynamic collection reference");
                }
            } else if t.get(i + 1) == Some(&Token::Punct('(')) {
                let Some((args, end)) = arguments(t, i + 2) else {
                    self.opaque(e, scope, "incomplete expression");
                    return;
                };
                let lookup = ["Lookup", "LookupSet", "MultiLookup"]
                    .iter()
                    .any(|f| word.eq_ignore_ascii_case(f));
                // Only aggregate, scope and lookup functions take dataset names.
                let scope_arg = match word.to_ascii_lowercase().as_str() {
                    "lookup" | "lookupset" | "multilookup" => Some(3),
                    "first" | "last" | "sum" | "avg" | "count" | "countdistinct" | "max"
                    | "min" | "stdev" | "stdevp" | "var" | "varp" | "aggregate" | "previous" => {
                        Some(1)
                    }
                    "runningvalue" => Some(2),
                    "countrows" | "rownumber" | "inscope" => Some(0),
                    _ => None,
                };
                let explicit = scope_arg.and_then(|n| args.get(n)).copied();
                let target = match explicit {
                    Some([Token::Literal(name)]) => self.scopes.get(*name).cloned(),
                    _ => None,
                };
                let inherit = explicit.is_none()
                    || matches!(explicit,Some([Token::Word(w)]) if w.eq_ignore_ascii_case("Nothing"));
                if let Some(s) = &target {
                    self.reference(LocalObject::Dataset(s.clone()), owner, e, false);
                } else if !inherit {
                    self.opaque(e, None, "unresolved expression scope");
                }
                let target = target.as_deref();
                for (n, arg) in args.iter().enumerate() {
                    let local = if lookup {
                        if n == 1 || n == 2 { target } else { scope }
                    } else {
                        if inherit { scope } else { target }
                    };
                    self.expression(arg, local, owner, e, filter);
                }
                i = end;
            }
            i += 1;
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Literal(&'a str),
    Punct(char),
}

fn tokens(mut s: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    while let Some(ch) = s.chars().next() {
        if ch == '\'' {
            break;
        }
        if ch == '"' {
            s = &s[1..];
            let mut end = 0;
            while let Some(pos) = s[end..].find('"') {
                end += pos;
                if s[end + 1..].starts_with('"') {
                    end += 2;
                } else {
                    break;
                }
            }
            if !s[end..].starts_with('"') {
                break;
            }
            out.push(Token::Literal(&s[..end]));
            s = &s[end + 1..];
        } else if ch.is_alphanumeric() || ch == '_' {
            let end = s
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(s.len());
            out.push(Token::Word(&s[..end]));
            s = &s[end..];
        } else {
            if !ch.is_whitespace() {
                out.push(Token::Punct(ch));
            }
            s = &s[ch.len_utf8()..];
        }
    }
    out
}

fn arguments<'a, 'b>(t: &'a [Token<'b>], start: usize) -> Option<(Vec<&'a [Token<'b>]>, usize)> {
    let mut args = vec![];
    let mut begin = start;
    let mut depth = 0;
    for (i, token) in t.iter().enumerate().skip(start) {
        match token {
            Token::Punct('(') => {
                depth += 1;
                if depth > 128 {
                    return None;
                }
            }
            Token::Punct(')') if depth == 0 => {
                args.push(&t[begin..i]);
                return Some((args, i));
            }
            Token::Punct(')') => depth -= 1,
            Token::Punct(',') if depth == 0 => {
                args.push(&t[begin..i]);
                begin = i + 1;
            }
            _ => {}
        }
    }
    None
}
