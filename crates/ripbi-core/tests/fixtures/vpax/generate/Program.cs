// Regenerates tiny.vpax and tiny-model-only.vpax with the real Dax.Vpax library
// (the one DAX Studio and semantic-link-labs use): `dotnet run -- ..` from this folder.
// tests/vpax_fixtures.rs asserts the hand-computed sizes below.

using Dax.Metadata;
using Dax.Vpax.Tools;

var model = new Model
{
    ModelName = new DaxName("Tiny"),
    ServerName = new DaxName("localhost:1234"),
    ExtractionDate = new DateTime(2026, 7, 14, 12, 0, 0, DateTimeKind.Utc),
};

var sales = Table(model, "Sales", 1000);
var s0 = Part(sales, "Sales-2025");
var s1 = Part(sales, "Sales-2026");
var amount = Col(sales, "Amount", 900, 400, "VALUE");
Seg(amount, s0, 0, 600, 600); Seg(amount, s1, 0, 400, 300); Hier(amount, s0, 50);
var salesKey = Col(sales, "ProductKey", 100, 100, "HASH");
Seg(salesKey, s0, 0, 600, 150); Seg(salesKey, s1, 0, 400, 50); Hier(salesKey, s0, 40);
var rowNumber = Col(sales, "RowNumber-2662979B-1795-4F74-8F37-6A1BA8059B61", 1000, 0, "VALUE");
rowNumber.IsRowNumber = true;
Seg(rowNumber, s0, 0, 600, 12); Seg(rowNumber, s1, 0, 400, 4);

var product = Table(model, "Product", 100);
var p0 = Part(product, "Product");
var productKey = Col(product, "ProductKey", 100, 50, "HASH");
Seg(productKey, p0, 0, 100, 80);
var category = Col(product, "Category", 5, 30, "HASH");
Seg(category, p0, 0, 100, 20);

model.Relationships.Add(new Relationship(salesKey, productKey)
{
    FromCardinalityType = "Many", ToCardinalityType = "One",
    CrossFilteringBehavior = "OneDirection", IsActive = true, UsedSizeFrom = 24,
});

var outDir = args[0];
using (var f = File.Create(Path.Combine(outDir, "tiny.vpax"))) VpaxTools.ExportVpax(f, model, new Dax.ViewVpaExport.Model(model), null);
using (var f = File.Create(Path.Combine(outDir, "tiny-model-only.vpax"))) VpaxTools.ExportVpax(f, model, null, null);
Console.WriteLine("ok");

static Table Table(Model m, string name, long rows)
{
    var t = new Table(m) { TableName = new DaxName(name), RowsCount = rows, IsReferenced = true };
    m.Tables.Add(t); return t;
}
static Partition Part(Table t, string name)
{
    var p = new Partition(t) { PartitionName = new DaxName(name), State = Partition.PartitionState.Read, Type = Partition.PartitionType.M, Mode = Partition.PartitionMode.Import };
    t.Partitions.Add(p); return p;
}
static Column Col(Table t, string name, long card, long dict, string enc)
{
    var c = new Column(t) { ColumnName = new DaxName(name), ColumnCardinality = card, DataType = "Int64", Encoding = enc, DictionarySize = dict, State = "Ready", IsReferenced = true };
    t.Columns.Add(c); return c;
}
static void Seg(Column c, Partition p, long n, long rows, long size)
    => c.ColumnSegments.Add(new ColumnSegment(c, p) { SegmentNumber = n, SegmentRows = rows, UsedSize = size });
static void Hier(Column c, Partition p, long size)
    => c.ColumnHierarchies.Add(new ColumnHierarchy(c) { StructureName = new DaxName("H$" + c.ColumnName.Name + " (1)$POS_TO_ID"), UsedSize = size });
