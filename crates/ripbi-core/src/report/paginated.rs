//! Report-local names live in their own namespace. A dataset field may be an
//! alias or a calculated field, even when a model has a column of the same name.
use super::ReportQuery;

/// The declarations and expressions of one report definition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaginatedReport {
    /// Declared objects in document order.
    pub objects: Vec<Declaration>,
    /// Local references and their consuming sites.
    pub references: Vec<Reference>,
    /// Connection identities used when pairing a model.
    pub sources: Vec<Source>,
    /// Dataset commands, including those for non-tabular sources.
    pub queries: Vec<DatasetQuery>,
    /// Model dependencies are indeterminate for one or more paired datasets.
    pub incomplete_model: bool,
}

/// A name qualified by its RDL collection and dataset scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LocalObject {
    /// Named data source.
    Source(String),
    /// Named report dataset.
    Dataset(String),
    /// A field qualified by its declaring dataset.
    Field(String, String),
    /// Report parameter.
    Parameter(String),
}

impl LocalObject {
    /// Stable scan and inventory kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Source(_) => "rdl_source",
            Self::Dataset(_) => "rdl_dataset",
            Self::Field(_, _) => "rdl_field",
            Self::Parameter(_) => "rdl_parameter",
        }
    }

    /// Human-readable qualified name.
    pub fn label(&self) -> String {
        match self {
            Self::Source(name) | Self::Dataset(name) | Self::Parameter(name) => name.clone(),
            Self::Field(dataset, name) => format!("{dataset}.{name}"),
        }
    }
}

/// A local declaration and its source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The declared name.
    pub object: LocalObject,
    /// One-based source line.
    pub line: usize,
    /// External inputs or opaque expressions can consume this declaration.
    pub used: bool,
    /// Returned field identifier as written in DataField.
    pub data_field: Option<String>,
}

/// A statically named reference in a report expression or property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The referenced local name.
    pub target: LocalObject,
    /// Nearest named report item.
    pub owner: String,
    /// Declaration whose evaluation reads this reference, if any.
    pub consumer: Option<LocalObject>,
    /// RDL property containing the reference.
    pub property: String,
    /// One-based source line.
    pub line: usize,
    /// A filter names an absent declaration in an inspected scope.
    pub broken_filter: bool,
}

/// Identity of a report data connection. Credentials are discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Connection name as written.
    pub name: String,
    /// Whether the provider accepts tabular model queries.
    pub semantic: bool,
    /// Connection details live in an external shared definition.
    pub shared: bool,
    /// Initial catalog, when statically declared.
    pub catalog: Option<String>,
    /// Service item identity when the connection records it.
    pub model_id: Option<String>,
}

/// The command a dataset issues through a named connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetQuery {
    /// Owning dataset.
    pub dataset: String,
    /// Named report connection.
    pub source: String,
    /// Command text as written.
    pub text: String,
    /// One-based source line.
    pub line: usize,
}

impl Reference {
    /// Stable diagnostic code for a missing filter target.
    pub fn missing_reason(&self) -> &'static str {
        match self.target {
            LocalObject::Parameter(_) => "rdl_parameter_not_found",
            _ => "rdl_field_not_found",
        }
    }
}

impl Source {
    /// An explicit service identity takes precedence over display names.
    pub fn matches(&self, names: &[&str], model_id: Option<&str>) -> bool {
        if let (Some(id), Some(target)) = (&self.model_id, model_id) {
            return id.eq_ignore_ascii_case(target);
        }
        self.catalog.as_deref().is_some_and(|c| {
            names
                .iter()
                .copied()
                .chain(model_id)
                .any(|n| c.eq_ignore_ascii_case(n))
        })
    }
}

impl PaginatedReport {
    /// Pair each static command independently. Return the datasets whose possible
    /// consumption of the model could not be inspected.
    pub fn model_queries(
        &self,
        names: &[&str],
        model_id: Option<&str>,
        explicit: bool,
    ) -> (Vec<ReportQuery>, Vec<&DatasetQuery>) {
        let semantic: Vec<_> = self.sources.iter().filter(|s| s.semantic).collect();
        let has_match = semantic.iter().any(|s| s.matches(names, model_id));
        let mut queries = Vec::new();
        let mut unknown = Vec::new();
        for query in &self.queries {
            let Some(source) = self.sources.iter().find(|s| s.name == query.source) else {
                unknown.push(query);
                continue;
            };
            if source.shared {
                unknown.push(query);
                continue;
            }
            if !source.semantic {
                continue;
            }
            let known_other = matches!((&source.model_id,model_id),(Some(id),Some(target)) if !id.eq_ignore_ascii_case(target));
            if known_other {
                continue;
            }
            if !(source.matches(names, model_id) || explicit && semantic.len() == 1) {
                if source.catalog.is_none() || explicit && !has_match {
                    unknown.push(query);
                }
                continue;
            }
            if query.text.trim().is_empty() || query.text.trim_start().starts_with('=') {
                unknown.push(query);
                continue;
            }
            queries.push(ReportQuery {
                dataset: Some(query.dataset.clone()),
                connection: source.name.clone(),
                text: query.text.clone(),
            });
        }
        (queries, unknown)
    }
}
