# Archive ingestion fixtures

`legacy-layout.pbix`, `missing-m.pbit`, and `mashup-fallback.pbit` are
synthetic archives generated for the tests. `layout-source.json` is the
readable source for the UTF-16 legacy layout fixture. Its structure was
checked against the public
[`old-Retail-Analysis-Sample-PBIX.pbix`](https://github.com/Hugoberry/pbixray/blob/master/data/old-Retail-Analysis-Sample-PBIX.pbix)
fixture: that archive also has a UTF-16 `Report/Layout` containing sections,
visual containers, and stringified visual config and query JSON. The public
archive is used only for manual validation and is not checked in here.

`hierarchy-alias.pbix` is generated from `hierarchy-alias.json` (visual
`config` stringified, the whole layout encoded as UTF-16LE). Its aliased
`HierarchyLevel` shape mirrors the legacy Layout of Microsoft's
`Regional Sales Sample.pbix` (issue #126).

`modern-report.pbix` contains only the PBIR report definition from the public
Microsoft AdventureWorks PBIT export. Its source and MIT attribution are in
[`samples/README.md`](../../../../../samples/README.md).
