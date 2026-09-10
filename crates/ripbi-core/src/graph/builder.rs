//! The edge catalog: how the dependency graph is constructed from the model and
//! report ASTs.
//!
//! Everything flows through the sanctioned enumerations —
//! [`TabularDatabase::dax_expressions`], [`TabularDatabase::m_expressions`],
//! [`ReportModel::bindings`], and [`ReportModel::dax_expressions`] — never by
//! walking the AST, so a new expression or binding site cannot be silently
//! omitted from the graph.
//!
//! Resolution follows the conservatism rule: an unqualified `[Name]` keeps every
//! candidate alive (never [`UnqualifiedMatches::primary`]), and beyond the
//! binder's own answers a qualified `'Table'[Name]` also keeps a same-named
//! hierarchy and calculation item alive. A qualified reference that matches
//! nothing keeps its qualifying table alive — the nearest resolvable candidate
//! the written form asserts. All of these can only over-mark usage, which is
//! the safe direction.

use std::collections::{HashMap, HashSet};

use petgraph::graph::{DiGraph, NodeIndex};

use crate::dax::{self, RawRef, unescape_name};
use crate::identity::{NameKey, ObjectId, fold_name};
use crate::model::index::{ModelIndex, Resolved, UnqualifiedMatches};
use crate::model::{
    ColumnKind, DaxExpressionRef, Hierarchy, HierarchyRef, Relationship, Table, TabularDatabase,
    Variation,
};
use crate::report::{BindingKind, FieldTarget, ReportModel};

use super::DependencyGraph;
use super::provenance::{BindingEdge, BindingSite, Provenance, StructuralEdge};

/// Assembles the graph for one model and every report sharing it. Never fails:
/// resolution misses are data (an edge that cannot be created is skipped), and
/// an empty report slice leaves every model object unused.
pub(in crate::graph) fn build(db: &TabularDatabase, reports: &[&ReportModel]) -> DependencyGraph {
    let index = ModelIndex::build(db);
    let mut builder = Builder {
        graph: DiGraph::new(),
        nodes: HashMap::new(),
        edges: HashSet::new(),
        roots: Vec::new(),
        root_set: HashSet::new(),
        m_named: HashMap::new(),
    };

    builder.add_model_objects(db, reports);
    builder.add_structural_edges(db, &index);
    builder.add_dax_edges(db, &index, reports);
    builder.add_m_edges(db, &index);
    for report in reports {
        builder.add_roots(db, &index, report);
    }

    builder.finish()
}

/// One in-progress graph: the petgraph itself, the node-key index, the dedup
/// set, and the reachability roots.
struct Builder {
    graph: DiGraph<ObjectId, Provenance>,
    nodes: HashMap<ObjectId, NodeIndex>,
    edges: HashSet<(NodeIndex, NodeIndex, Provenance)>,
    roots: Vec<(ObjectId, Provenance)>,
    root_set: HashSet<(ObjectId, Provenance)>,
    /// Columns named by M expressions — the supply chain, deliberately not
    /// edges. Key: the column. Value: the expressions that name it.
    m_named: HashMap<ObjectId, Vec<ObjectId>>,
}

impl Builder {
    /// The node for `id`, creating it on first use. Every model object is
    /// pre-created by [`Builder::add_model_objects`], so this only backfills
    /// edge endpoints that resolve late.
    fn node(&mut self, id: &ObjectId) -> NodeIndex {
        if let Some(&index) = self.nodes.get(id) {
            return index;
        }
        let index = self.graph.add_node(id.clone());
        self.nodes.insert(id.clone(), index);
        index
    }

    /// Adds `from → to` with its provenance. Identical triples are deduped —
    /// an expression referencing the same column twice is one edge — while
    /// distinct provenance for the same pair is kept as parallel edges.
    /// Self-references keep nothing alive and are dropped.
    fn edge(&mut self, from: &ObjectId, to: &ObjectId, provenance: Provenance) {
        if from == to {
            return;
        }
        let source = self.node(from);
        let target = self.node(to);
        if self.edges.insert((source, target, provenance.clone())) {
            self.graph.add_edge(source, target, provenance);
        }
    }

    /// Records a reachability root. Report sites (visuals, pages, bookmarks) are
    /// not model objects, so roots live beside the object-to-object edges with
    /// their binding provenance.
    fn root(&mut self, target: ObjectId, provenance: Provenance) {
        if self.root_set.insert((target.clone(), provenance.clone())) {
            self.roots.push((target, provenance));
        }
    }

    fn finish(self) -> DependencyGraph {
        DependencyGraph::assemble(self.graph, self.nodes, self.roots, self.m_named)
    }

    /// Pre-creates a node for every model and report object, so isolated
    /// objects — a table nothing references, a function nobody calls — still
    /// exist in the graph and can be reported unused.
    fn add_model_objects(&mut self, db: &TabularDatabase, reports: &[&ReportModel]) {
        for table in &db.tables {
            let table_name = NameKey::new(&table.name);
            self.node(&ObjectId::Table {
                table: table_name.clone(),
            });
            for column in &table.columns {
                self.node(&ObjectId::Column {
                    table: table_name.clone(),
                    column: NameKey::new(&column.name),
                });
            }
            for measure in &table.measures {
                self.node(&ObjectId::Measure {
                    table: table_name.clone(),
                    measure: NameKey::new(&measure.name),
                });
            }
            for partition in &table.partitions {
                self.node(&ObjectId::Partition {
                    table: table_name.clone(),
                    partition: NameKey::new(&partition.name),
                });
            }
            for hierarchy in &table.hierarchies {
                self.node(&ObjectId::Hierarchy {
                    table: table_name.clone(),
                    hierarchy: NameKey::new(&hierarchy.name),
                });
            }
            if let Some(group) = &table.calculation_group {
                for item in &group.items {
                    self.node(&ObjectId::CalculationItem {
                        table: table_name.clone(),
                        item: NameKey::new(&item.name),
                    });
                }
            }
        }
        for relationship in &db.relationships {
            self.node(&relationship_node_id(relationship));
        }
        for role in &db.roles {
            self.node(&ObjectId::Role {
                role: NameKey::new(&role.name),
            });
        }
        for expression in &db.expressions {
            self.node(&ObjectId::Expression {
                name: NameKey::new(&expression.name),
            });
        }
        for function in &db.functions {
            self.node(&ObjectId::Function {
                name: NameKey::new(&function.name),
            });
        }
        for report in reports {
            for measure in &report.measures {
                self.node(&ObjectId::ReportMeasure {
                    measure: measure.name.clone(),
                });
            }
        }
    }

    /// The structural edges: containment and engine-managed liveness that flows
    /// with no written reference anywhere.
    fn add_structural_edges(&mut self, db: &TabularDatabase, index: &ModelIndex) {
        for table in &db.tables {
            let table_name = NameKey::new(&table.name);
            let table_id = ObjectId::Table {
                table: table_name.clone(),
            };

            for column in &table.columns {
                let column_id = ObjectId::Column {
                    table: table_name.clone(),
                    column: NameKey::new(&column.name),
                };
                // A used column keeps its table alive.
                self.edge(&column_id, &table_id, member());

                // Columns materialized by the table's own DAX cannot be dropped
                // independently of it.
                if column.kind == ColumnKind::CalculatedTableColumn {
                    self.edge(&table_id, &column_id, engine_managed());
                }
                // A used column keeps its sort-by and group-by columns alive.
                if let Some(target) = column
                    .sort_by_column
                    .as_deref()
                    .and_then(|sort| same_table_column(table, sort))
                {
                    self.edge(&column_id, &target, sort_by());
                }
                for group in &column.group_by_columns {
                    if let Some(target) = same_table_column(table, group) {
                        self.edge(&column_id, &target, group_by());
                    }
                }
            }

            // A calculation group's columns (its field and ordinal columns) are
            // engine-managed like calculated-table columns, and a used
            // calculation item keeps the group's table alive.
            if let Some(group) = &table.calculation_group {
                for column in &table.columns {
                    self.edge(
                        &table_id,
                        &ObjectId::Column {
                            table: table_name.clone(),
                            column: NameKey::new(&column.name),
                        },
                        engine_managed(),
                    );
                }
                for item in &group.items {
                    self.edge(
                        &ObjectId::CalculationItem {
                            table: table_name.clone(),
                            item: NameKey::new(&item.name),
                        },
                        &table_id,
                        member(),
                    );
                }
            }
            for calendar in &table.calendars {
                for name in &calendar.columns {
                    if let Some(target) = same_table_column(table, name) {
                        self.edge(&table_id, &target, engine_managed());
                    }
                }
            }

            for measure in &table.measures {
                self.edge(
                    &ObjectId::Measure {
                        table: table_name.clone(),
                        measure: NameKey::new(&measure.name),
                    },
                    &table_id,
                    member(),
                );
            }
            // A used table loads through its partitions.
            for partition in &table.partitions {
                self.edge(
                    &table_id,
                    &ObjectId::Partition {
                        table: table_name.clone(),
                        partition: NameKey::new(&partition.name),
                    },
                    table_partition(),
                );
            }
            for hierarchy in &table.hierarchies {
                let hierarchy_id = ObjectId::Hierarchy {
                    table: table_name.clone(),
                    hierarchy: NameKey::new(&hierarchy.name),
                };
                // A used hierarchy keeps its table alive…
                self.edge(&hierarchy_id, &table_id, member());
                // …and drills down through its levels' columns.
                for level in &hierarchy.levels {
                    if let Some(target) = same_table_column(table, &level.column) {
                        self.edge(&hierarchy_id, &target, hierarchy_level());
                    }
                }
            }
        }

        for rel in &db.relationships {
            let id = relationship_node_id(rel);
            let from = endpoint_column(db, index, &rel.from_table, &rel.from_column);
            let to = endpoint_column(db, index, &rel.to_table, &rel.to_column);
            if rel.is_active {
                // Either endpoint table keeps the relationship alive, and it
                // keeps both key columns alive — but, being a relationship
                // endpoint, without keeping their tables alive.
                for (table_id, _) in [&from, &to].into_iter().flatten() {
                    self.edge(table_id, &id, relationship());
                }
                for (_, column_id) in [&from, &to].into_iter().flatten() {
                    self.edge(&id, column_id, relationship_endpoint());
                }
            } else {
                // An inactive relationship is recorded on both sides without
                // any liveness: only a live `USERELATIONSHIP` call — an
                // activation edge from the DAX walk — can switch it on, so an
                // unactivated one is a finding, and its keys are findings
                // that point back at it.
                for (table_id, _) in [&from, &to].into_iter().flatten() {
                    self.edge(table_id, &id, inactive_relationship());
                }
                for (_, column_id) in [&from, &to].into_iter().flatten() {
                    self.edge(&id, column_id, inactive_relationship_endpoint());
                }
            }
        }

        for role in &db.roles {
            let role_id = ObjectId::Role {
                role: NameKey::new(&role.name),
            };
            for permission in &role.table_permissions {
                // A role grants access to the table; while the role exists, the
                // table is referenced.
                if let Some(table_id) = table_node(db, index, &permission.table) {
                    self.edge(&role_id, &table_id, role_permission());
                }
            }
        }
    }

    /// Edges from every DAX expression — model-side and report-side — to the
    /// objects its references resolve to.
    fn add_dax_edges(
        &mut self,
        db: &TabularDatabase,
        index: &ModelIndex,
        reports: &[&ReportModel],
    ) {
        for expression in db.dax_expressions() {
            self.add_expression_edges(db, index, &expression, None);
        }
        for report in reports {
            for expression in report.dax_expressions() {
                self.add_expression_edges(db, index, &expression, Some(report));
            }
        }
    }

    /// Edges for one expression's references. A report-measure body additionally
    /// keeps same-named report measures alive: they live in the report, not the
    /// model, so the binder cannot see them.
    fn add_expression_edges<'a>(
        &mut self,
        db: &'a TabularDatabase,
        index: &ModelIndex,
        expression: &DaxExpressionRef<'a>,
        report: Option<&ReportModel>,
    ) {
        let owner = expression.owner.to_object_id();
        let site = DaxSite {
            owner: &owner,
            provenance: Provenance::Dax {
                kind: expression.kind,
            },
        };

        let refs = dax::references(expression.text);
        for (position, raw) in refs.iter().enumerate() {
            // `USERELATIONSHIP('A'[X], 'B'[Y])` is the only DAX that can
            // switch an inactive relationship on at query time — the
            // activation edge is what keeps the relationship alive. The two
            // arguments bind to their columns through the ordinary walk
            // below; the extraction emits the call and its qualified field
            // arguments adjacently.
            if let (Some((a, b)), RawRef::Function { name, .. }) =
                (refs.get(position + 1).zip(refs.get(position + 2)), &raw)
                && fold_name(name) == "userelationship"
            {
                for target in userelationship_targets(db, a, b) {
                    self.edge(site.owner, &target, site.provenance.clone());
                }
            }

            if let (
                Some(report),
                RawRef::Field {
                    table: None, name, ..
                },
            ) = (report, &raw)
            {
                let folded = fold_name(unescape_name(name).as_ref());
                for measure in report
                    .measures
                    .iter()
                    .filter(|measure| fold_name(measure.name.as_str()) == folded)
                {
                    self.edge(
                        site.owner,
                        &ObjectId::ReportMeasure {
                            measure: measure.name.clone(),
                        },
                        site.provenance.clone(),
                    );
                }
            }

            if let RawRef::Field {
                table: Some(table),
                name,
                ..
            } = &raw
            {
                // The binder's answer: the table's column, falling back to the
                // model-global measure for a stale or wrong qualifier.
                let binding = dax::bind(db, index, expression.home_table, raw.clone());
                for target in binding.targets() {
                    self.edge(site.owner, target, site.provenance.clone());
                }
                // Extended candidates the binder does not know, and the table
                // fallback for a reference that matched nothing at all.
                self.extend_qualified(db, index, &site, table, name, !binding.is_unresolved());
            } else {
                let binding = dax::bind(db, index, expression.home_table, raw.clone());
                for target in binding.targets() {
                    self.edge(site.owner, target, site.provenance.clone());
                }
            }
        }
    }

    /// The unresolved-reference policy for one qualified `'Table'[Name]`:
    /// hierarchies and calculation items are candidates the binder does not
    /// know about, and a reference matching nothing keeps its qualifying table
    /// alive — the nearest resolvable candidate the written form asserts.
    fn extend_qualified(
        &mut self,
        db: &TabularDatabase,
        index: &ModelIndex,
        site: &DaxSite<'_>,
        table: &str,
        name: &str,
        binder_resolved: bool,
    ) {
        let table = unescape_name(table);
        let name = unescape_name(name);
        let folded = fold_name(&name);
        let mut extended = false;

        if let Some(t) = index
            .resolve_table(&table)
            .and_then(|handle| db.table(handle))
        {
            if let Some(hierarchy) = t.hierarchies.iter().find(|h| fold_name(&h.name) == folded) {
                self.edge(
                    site.owner,
                    &ObjectId::Hierarchy {
                        table: NameKey::new(&t.name),
                        hierarchy: NameKey::new(&hierarchy.name),
                    },
                    site.provenance.clone(),
                );
                extended = true;
            }
            if let Some(group) = &t.calculation_group {
                for item in group
                    .items
                    .iter()
                    .filter(|item| fold_name(&item.name) == folded)
                {
                    self.edge(
                        site.owner,
                        &ObjectId::CalculationItem {
                            table: NameKey::new(&t.name),
                            item: NameKey::new(&item.name),
                        },
                        site.provenance.clone(),
                    );
                    extended = true;
                }
            }
        }

        if !extended
            && !binder_resolved
            && let Some(table_id) = table_node(db, index, &table)
        {
            self.edge(site.owner, &table_id, site.provenance.clone());
        }
    }

    /// Edges from M expressions to the model objects they reference: the
    /// shared expressions, tables, and columns named by a partition's or
    /// shared expression's Power Query text.
    ///
    /// A column referenced only inside Power Query is refresh-critical —
    /// removing it breaks refresh even though no DAX or report binding touches
    /// it — so its reference keeps it alive, and a merge-source table keeps
    /// the table it merges in. The edges stay ordinary [`Provenance::M`] edges,
    /// so liveness still flows through the owner: a dead table's partition
    /// keeps nothing alive, and a reference never marks anything live on its
    /// own.
    /// Wires the Power Query references into the graph. The pipeline is M →
    /// tables/columns → DAX → reports: deleting a model object can never break
    /// what is upstream of it, and M is upstream of everything. What the two
    /// directions mean here:
    ///
    /// - a **column** named in M is that column's supply chain — the query
    ///   keeps producing it and the model stops mapping it. Unloading the
    ///   column cannot break refresh, so there is deliberately *no* liveness
    ///   edge; the naming expression is recorded in `m_named` instead, so
    ///   findings can say what a *full* removal — script included — must edit.
    ///   Only Data columns qualify: an M step can only name a column it
    ///   produces, so a calculated column (or a calculated-table column)
    ///   matching an M name is coincidence, not supply chain — the auto
    ///   date/time columns named like Desktop's date-template query, for one;
    /// - a **table or shared expression** named in M is different: deleting it
    ///   deletes the query this expression reads or joins, which breaks
    ///   refresh. Those stay real edges.
    fn add_m_edges(&mut self, db: &TabularDatabase, index: &ModelIndex) {
        for expression in db.m_expressions() {
            let owner = expression.owner.to_object_id();
            for binding in crate::m::bindings(db, index, expression.text) {
                for target in binding.targets() {
                    match target {
                        ObjectId::Column { table, column } => {
                            let produced_by_m = index
                                .resolve_qualified(table.as_str(), column.as_str())
                                .and_then(|resolved| match resolved {
                                    Resolved::Column(handle) => db.column(handle),
                                    Resolved::Measure(_) => None,
                                })
                                .is_some_and(|column| matches!(column.kind, ColumnKind::Data));
                            if produced_by_m {
                                self.m_named
                                    .entry(target.clone())
                                    .or_default()
                                    .push(owner.clone());
                            }
                        }
                        ObjectId::Table { table } => {
                            // A partition naming its own table says nothing
                            // new, and the edge would cycle with the
                            // table-partition edge.
                            if let ObjectId::Partition { table: own, .. } = &owner
                                && own == table
                            {
                                continue;
                            }
                            self.edge(&owner, target, Provenance::M);
                        }
                        ObjectId::Expression { .. } => {
                            self.edge(&owner, target, Provenance::M);
                        }
                        _ => {}
                    }
                }
            }
        }
        for owners in self.m_named.values_mut() {
            owners.sort();
            owners.dedup();
        }
    }

    /// The root edges of one report: every binding target, with the binding's
    /// provenance.
    fn add_roots(&mut self, db: &TabularDatabase, index: &ModelIndex, report: &ReportModel) {
        let report_name = report.name.as_ref().map(NameKey::new);
        for binding in report.bindings() {
            let provenance = Provenance::Binding(Box::new(BindingEdge {
                kind: binding_site(binding.kind),
                report: report_name.clone(),
                page: binding.page.cloned(),
                visual: binding.visual.cloned(),
                bookmark: binding.bookmark.cloned(),
            }));
            for target in self.field_target_targets(db, index, report, binding.target) {
                self.root(target.clone(), provenance.clone());
                self.add_selection_edges(db, &target, provenance.clone());
            }
        }
    }

    /// A report binding that lands on a calculation-group column can select
    /// any of the group's items by name at query time — a slicer over the
    /// field column, a filter naming an item — so the binding keeps every
    /// item of the group alive too. Written uses only: structural liveness
    /// of the group (its table alive through one explicitly named item)
    /// deliberately does not spread to the unselected items.
    fn add_selection_edges(
        &mut self,
        db: &TabularDatabase,
        target: &ObjectId,
        provenance: Provenance,
    ) {
        let ObjectId::Column { table, .. } = target else {
            return;
        };
        let Some(group) = db
            .tables
            .iter()
            .find(|t| NameKey::new(&t.name) == *table)
            .and_then(|t| t.calculation_group.as_ref())
        else {
            return;
        };
        for item in &group.items {
            self.edge(
                target,
                &ObjectId::CalculationItem {
                    table: table.clone(),
                    item: NameKey::new(&item.name),
                },
                provenance.clone(),
            );
        }
    }

    /// Every resolved target of one written report binding.
    fn field_target_targets(
        &self,
        db: &TabularDatabase,
        index: &ModelIndex,
        report: &ReportModel,
        target: &FieldTarget,
    ) -> Vec<ObjectId> {
        match target {
            FieldTarget::Column { table, column } => {
                self.qualified_targets(db, index, table, column)
            }
            FieldTarget::Measure { measure, .. } => {
                // Within its report, a report measure shadows a model measure of
                // the same name.
                match report_measure(report, measure) {
                    Some(id) => vec![id],
                    None => model_measure(db, index, measure),
                }
            }
            FieldTarget::HierarchyLevel {
                table,
                hierarchy,
                level,
                via_column,
                via_variation,
            } => {
                let mut out = Vec::new();
                if let Some(t) = table_struct(db, index, table.as_str()) {
                    if let Some(h) = t
                        .hierarchies
                        .iter()
                        .find(|h| NameKey::new(&h.name) == *hierarchy)
                    {
                        return hierarchy_targets(t, h, level);
                    }
                    // A hierarchy reached over a column variation names the
                    // *varied* table, but the hierarchy lives on the
                    // variation's target — resolve the declaration before
                    // giving up on the binding.
                    if let Some(via_column) = via_column
                        && let Some(targets) = variation_hierarchy_targets(
                            db,
                            index,
                            t,
                            via_column,
                            via_variation.as_ref(),
                            hierarchy,
                            level,
                        )
                    {
                        return targets;
                    }
                    // A hierarchy binding naming a table with no such hierarchy
                    // still keeps the table alive.
                    out.push(ObjectId::Table {
                        table: NameKey::new(&t.name),
                    });
                }
                out
            }
            FieldTarget::Aggregation { inner, .. } => {
                self.field_target_targets(db, index, report, inner)
            }
            FieldTarget::Written(reference) => match &reference.table {
                Some(table) => self.qualified_targets(db, index, table, &reference.name),
                None => match report_measure(report, &reference.name) {
                    Some(id) => vec![id],
                    None => model_measure(db, index, &reference.name),
                },
            },
        }
    }

    /// Every candidate of a written qualified reference against the model: the
    /// column-or-measure binder rule, plus same-named hierarchies and
    /// calculation items, plus the table fallback when nothing matched.
    fn qualified_targets(
        &self,
        db: &TabularDatabase,
        index: &ModelIndex,
        table: &NameKey,
        field: &NameKey,
    ) -> Vec<ObjectId> {
        let mut out = Vec::new();
        if let Some(id) = index
            .resolve_qualified(table.as_str(), field.as_str())
            .and_then(|resolved| db.object_id(resolved))
        {
            out.push(id);
        }
        if let Some(t) = table_struct(db, index, table.as_str()) {
            if let Some(hierarchy) = t
                .hierarchies
                .iter()
                .find(|h| NameKey::new(&h.name) == *field)
            {
                out.push(ObjectId::Hierarchy {
                    table: NameKey::new(&t.name),
                    hierarchy: NameKey::new(&hierarchy.name),
                });
            }
            if let Some(group) = &t.calculation_group {
                for item in group
                    .items
                    .iter()
                    .filter(|item| NameKey::new(&item.name) == *field)
                {
                    out.push(ObjectId::CalculationItem {
                        table: NameKey::new(&t.name),
                        item: NameKey::new(&item.name),
                    });
                }
            }
        }
        if out.is_empty()
            && let Some(table_id) = table_node(db, index, table.as_str())
        {
            out.push(table_id);
        }
        out
    }
}

/// The edge site of one DAX expression: its owner node and the provenance
/// every edge leaving it carries.
struct DaxSite<'a> {
    owner: &'a ObjectId,
    provenance: Provenance,
}

/// The column of `table` named `name`, if it exists — for same-table
/// structural references (sort-by, group-by, hierarchy levels, calendars) and
/// relationship endpoints.
fn same_table_column(table: &Table, name: &str) -> Option<ObjectId> {
    let folded = fold_name(name);
    table
        .columns
        .iter()
        .find(|column| fold_name(&column.name) == folded)
        .map(|column| ObjectId::Column {
            table: NameKey::new(&table.name),
            column: NameKey::new(&column.name),
        })
}

/// The binding targets of a resolved hierarchy: the hierarchy node plus the
/// drilled level's column.
fn hierarchy_targets(table: &Table, hierarchy: &Hierarchy, level: &NameKey) -> Vec<ObjectId> {
    let mut out = vec![ObjectId::Hierarchy {
        table: NameKey::new(&table.name),
        hierarchy: NameKey::new(&hierarchy.name),
    }];
    // Drilling to a level uses the level's column too.
    if let Some(level_column) = hierarchy
        .levels
        .iter()
        .find(|l| NameKey::new(&l.name) == *level)
        .and_then(|l| same_table_column(table, &l.column))
    {
        out.push(level_column);
    }
    out
}

/// Resolves a hierarchy binding that arrived over a column variation
/// (`PropertyVariationSource`): the binding names the *varied* (base) table,
/// but the hierarchy lives on the variation's target — for auto date/time the
/// engine's hidden `LocalDateTable_*`.
///
/// The primary path is the model's own declaration: the varied column's
/// [`Variation`] names the relationship (and the default hierarchy)
/// realizing it. When no declaration matches — a serialization that carries
/// the relationship but dropped the variation object — the variation
/// relationship is found by shape instead: the one touching the varied column
/// whose other endpoint is a flagged auto date/time table. `None` when nothing
/// resolves; the caller keeps its own fallback.
fn variation_hierarchy_targets(
    db: &TabularDatabase,
    index: &ModelIndex,
    base: &Table,
    via_column: &NameKey,
    via_variation: Option<&NameKey>,
    hierarchy: &NameKey,
    level: &NameKey,
) -> Option<Vec<ObjectId>> {
    let column = base
        .columns
        .iter()
        .find(|column| NameKey::new(&column.name) == *via_column)?;

    // The declarations to try, most specific first: the variation the binding
    // named, then the column's remaining variations in source order.
    let mut variations: Vec<&Variation> = column.variations.iter().collect();
    if let Some(name) = via_variation {
        variations.sort_by_key(|variation| NameKey::new(&variation.name) != *name);
    }

    // Candidate target tables in try order, each carrying the default
    // hierarchy declared for it, if any.
    let mut candidates: Vec<(&Table, Option<&HierarchyRef>)> = Vec::new();
    for variation in &variations {
        if let Some(relationship) = &variation.relationship
            && let Some(rel) = db
                .relationships
                .iter()
                .find(|rel| rel.name.as_deref() == Some(relationship.as_str()))
            && let Some(table) = variation_endpoint(db, index, rel, base, &column.name)
        {
            push_candidate(&mut candidates, table, variation.default_hierarchy.as_ref());
        }
        if let Some(reference) = &variation.default_hierarchy
            && let Some(table) = table_struct(db, index, &reference.table)
        {
            push_candidate(&mut candidates, table, Some(reference));
        }
    }
    for rel in &db.relationships {
        if let Some(table) = variation_endpoint(db, index, rel, base, &column.name)
            && table.is_local_date_table
        {
            push_candidate(&mut candidates, table, None);
        }
    }

    for (table, default_ref) in candidates {
        let found = table
            .hierarchies
            .iter()
            .find(|h| NameKey::new(&h.name) == *hierarchy)
            .or_else(|| {
                // The binding named a hierarchy this table does not carry, but
                // the declaration names the table's default — the default is
                // what the engine binds the varied column to.
                let reference = default_ref
                    .filter(|reference| fold_name(&reference.table) == fold_name(&table.name))?;
                table
                    .hierarchies
                    .iter()
                    .find(|h| fold_name(&h.name) == fold_name(&reference.hierarchy))
            });
        if let Some(h) = found {
            return Some(hierarchy_targets(table, h, level));
        }
    }
    None
}

/// Adds a candidate target table unless it is already present; the variation's
/// default-hierarchy reference rides along when it points at this table.
fn push_candidate<'a>(
    candidates: &mut Vec<(&'a Table, Option<&'a HierarchyRef>)>,
    table: &'a Table,
    default_ref: Option<&'a HierarchyRef>,
) {
    if !candidates
        .iter()
        .any(|(seen, _)| fold_name(&seen.name) == fold_name(&table.name))
    {
        candidates.push((table, default_ref));
    }
}

/// The relationship's endpoint table away from the given base column — only
/// when that base column is one of the endpoints.
fn variation_endpoint<'a>(
    db: &'a TabularDatabase,
    index: &ModelIndex,
    rel: &Relationship,
    base: &Table,
    column: &str,
) -> Option<&'a Table> {
    let from_matches = fold_name(&rel.from_table) == fold_name(&base.name)
        && fold_name(&rel.from_column) == fold_name(column);
    let to_matches = fold_name(&rel.to_table) == fold_name(&base.name)
        && fold_name(&rel.to_column) == fold_name(column);
    let other = if from_matches {
        &rel.to_table
    } else if to_matches {
        &rel.from_table
    } else {
        return None;
    };
    table_struct(db, index, other)
}

/// The table node for a table name, if the table exists.
fn table_node(db: &TabularDatabase, index: &ModelIndex, name: &str) -> Option<ObjectId> {
    table_struct(db, index, name).map(|table| ObjectId::Table {
        table: NameKey::new(&table.name),
    })
}

/// The table struct for a table name, if the table exists.
fn table_struct<'a>(db: &'a TabularDatabase, index: &ModelIndex, name: &str) -> Option<&'a Table> {
    index
        .resolve_table(name)
        .and_then(|handle| db.table(handle))
}

/// The (table node, column node) pair of a relationship endpoint, only when
/// both exist — an endpoint keeps an existing column alive, never fabricates
/// one.
fn endpoint_column(
    db: &TabularDatabase,
    index: &ModelIndex,
    table: &str,
    column: &str,
) -> Option<(ObjectId, ObjectId)> {
    let table_id = table_node(db, index, table)?;
    let t = table_struct(db, index, table)?;
    let column_id = same_table_column(t, column)?;
    Some((table_id, column_id))
}

/// The model-global measure of that name, if one exists.
fn model_measure(db: &TabularDatabase, index: &ModelIndex, name: &NameKey) -> Vec<ObjectId> {
    let UnqualifiedMatches { measure, .. } = index.resolve_unqualified(name.as_str(), None);
    measure
        .and_then(|handle| db.object_id(Resolved::Measure(handle)))
        .into_iter()
        .collect()
}

/// The report measure of that name — a distinct node from any model measure.
fn report_measure(report: &ReportModel, name: &NameKey) -> Option<ObjectId> {
    report
        .measures
        .iter()
        .find(|measure| measure.name == *name)
        .map(|measure| ObjectId::ReportMeasure {
            measure: measure.name.clone(),
        })
}

/// Every model relationship whose endpoints are exactly the two qualified
/// columns of a `USERELATIONSHIP('A'[X], 'B'[Y])` call, in either argument
/// order. Unqualified arguments match nothing (they are invalid in a real
/// call), and an ambiguous pair activates every matching relationship —
/// keeping one more is the safe direction. Empty when the call names no
/// model relationship; the arguments still bind to their columns through the
/// ordinary walk.
fn userelationship_targets(db: &TabularDatabase, a: &RawRef, b: &RawRef) -> Vec<ObjectId> {
    let (
        RawRef::Field {
            table: Some(ft),
            name: fc,
            ..
        },
        RawRef::Field {
            table: Some(tt),
            name: tc,
            ..
        },
    ) = (a, b)
    else {
        return Vec::new();
    };
    let (from, to) = (
        (
            fold_name(unescape_name(ft).as_ref()),
            fold_name(unescape_name(fc).as_ref()),
        ),
        (
            fold_name(unescape_name(tt).as_ref()),
            fold_name(unescape_name(tc).as_ref()),
        ),
    );
    db.relationships
        .iter()
        .filter(|rel| {
            let (a_side, b_side) = (
                (fold_name(&rel.from_table), fold_name(&rel.from_column)),
                (fold_name(&rel.to_table), fold_name(&rel.to_column)),
            );
            (a_side == from && b_side == to) || (a_side == to && b_side == from)
        })
        .map(|rel| ObjectId::Relationship {
            from_table: NameKey::new(&rel.from_table),
            from_column: NameKey::new(&rel.from_column),
            to_table: NameKey::new(&rel.to_table),
            to_column: NameKey::new(&rel.to_column),
        })
        .collect()
}

fn relationship_node_id(relationship: &crate::model::Relationship) -> ObjectId {
    ObjectId::Relationship {
        from_table: NameKey::new(&relationship.from_table),
        from_column: NameKey::new(&relationship.from_column),
        to_table: NameKey::new(&relationship.to_table),
        to_column: NameKey::new(&relationship.to_column),
    }
}

fn binding_site(kind: BindingKind<'_>) -> BindingSite {
    match kind {
        BindingKind::FieldWell { role } => BindingSite::FieldWell {
            role: role.to_string(),
        },
        BindingKind::Filter => BindingSite::Filter,
        BindingKind::Sort => BindingSite::Sort,
        BindingKind::Drillthrough => BindingSite::Drillthrough,
        BindingKind::ConditionalFormatting => BindingSite::ConditionalFormatting,
        BindingKind::AltText => BindingSite::AltText,
    }
}

fn member() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::TableMember,
    }
}

fn table_partition() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::TablePartition,
    }
}

fn relationship() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::Relationship,
    }
}

fn relationship_endpoint() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::RelationshipEndpoint,
    }
}

fn inactive_relationship() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::InactiveRelationship,
    }
}

fn inactive_relationship_endpoint() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::InactiveRelationshipEndpoint,
    }
}

fn sort_by() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::SortByColumn,
    }
}

fn group_by() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::GroupByColumn,
    }
}

fn hierarchy_level() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::HierarchyLevel,
    }
}

fn engine_managed() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::EngineManaged,
    }
}

fn role_permission() -> Provenance {
    Provenance::Structural {
        role: StructuralEdge::RolePermission,
    }
}
