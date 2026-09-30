# Excel OLAP pivot fixtures

Workbooks whose pivot tables are live-connected to an OLAP cube, for the Excel
consumer ingest (pivot tables as report roots against a semantic model). No
parser reads them yet.

Both are Analysis Services *multidimensional* cubes, not Power BI semantic
models: the unique-name shapes (`[Measures].[X]`, `[Dimension].[Hierarchy]`,
`[Dimension].[Hierarchy].[Level]`) and the pivot/cache structure match what
Excel writes against a Power BI model, but the connection strings are
`Provider=MSOLAP` against on-premises test servers, not `pbiazure://` or
`powerbi://`, and the names follow multidimensional rather than Tabular
conventions. Neither has an OLAP slicer or timeline.

| Fixture | Cube | What it exercises |
|---|---|---|
| `olap-pivot-foodmart.xlsx` | FoodMart 2000 `Sales` | One pivot (`xl/pivotTables/pivotTable1.xml`) with every field area: row fields `[Customers].[Country]`, `[Customers].[Country1]`, `[Customers].[State Province]` and the named set `[TestSet]`; column field `[Gender].[Gender]` plus the Values pseudo-field (`x="-2"`); data fields `[Measures].[Profit]` and `[Measures].[Unit Sales]`; page field `[Marital Status].[Marital Status]`. The cache defines a calculated measure (`[Measures].[TestCalculatedMeasure]`, MDX `1`) and a named set (`'[Product].[All Products].Children'`). 21 cache hierarchies, 15 cache fields. |
| `olap-pivots-adventure-works-strict.xlsx` | Adventure Works DW `Adventure Works` | Five pivots, each on its own cache, each using only `[Measures].[Internet Sales Amount]`, `[Date].[Calendar Year].[Calendar Year]`, and `[Customer].[Country].[Country]` — while every cache lists **313** `cacheHierarchies` (about 100 of them measures). The case where the cache must not be read as roots. Saved in ISO/IEC 29500 Strict: parts use the `http://purl.oclc.org/ooxml/spreadsheetml/main` namespace, not the transitional `http://schemas.openxmlformats.org/spreadsheetml/2006/main`. |

Rows, columns, and data fields index `cacheFields`; a page field carries both
`fld` (into `cacheFields`) and `hier` (into `cacheHierarchies`).

## Source

Copied unmodified (only renamed) from Microsoft's Open XML SDK test assets,
<https://github.com/dotnet/Open-XML-SDK> at commit
`431ab05cf160248cc3885a4a766026d4f8243792`:

| Fixture | Upstream path under `test/DocumentFormat.OpenXml.Tests.Assets/assets/TestDataStorage/` | SHA-256 |
|---|---|---|
| `olap-pivot-foodmart.xlsx` | `v2FxTestFiles/spreadsheet/Pivot4.xlsx` | `ae1a228d33253a63a170b55dc77fb81db5e21b8833367e4f1e49b9f4b4edbc52` |
| `olap-pivots-adventure-works-strict.xlsx` | `O14ISOStrict/Excel/filter_type.xlsx` | `9e32e6f06bcec398a6b1d89d5564b8b2ff7c3ef93c61ec0ab3da6497e46bf4b1` |

These files are **not** part of ripbi's source code and are not covered by the
Apache-2.0/MIT dual license at the repository root. They are redistributed here
under the Open XML SDK's MIT license, reproduced below as that license requires.

---

The MIT License (MIT)

Copyright (c) .NET Foundation and Contributors

All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
