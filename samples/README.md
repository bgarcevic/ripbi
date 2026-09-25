# Samples

Microsoft Power BI Desktop sample projects (PBIP format) used as fixtures for
developing and manually testing `ripbi`.

`Revenue Opportunities.pbix` is Microsoft's public
`2026 Power BI Samples Revamp/Revenue Opportunities.pbix`, copied unmodified. Its
model exists only as the compressed `DataModel`, so the archive parity tests
compare it with the `Revenue Opportunities` PBIP conversion.

`Revenue Opportunities Features.pbix` and its PBIP are that sample after adding,
in Power BI Desktop, the model features the public samples lack: a calculation
group with selection expressions, two roles (a row filter and a hidden table),
a DAX user-defined function (compatibility level 1702), a KPI, a dynamic format
string, a detail-rows expression, and an M parameter. Desktop saved the PBIX
and the PBIP from the same session, so the parity test compares them field for
field.

`AdventureWorks Sales.pbit` was exported with Power BI Desktop from Microsoft's
public `2026 Power BI Samples Revamp/AdventureWorks Sales.pbix`. It is paired
with the AdventureWorks PBIP conversion in the archive parity tests. The PBIT
contains the model schema and report definition, without the PBIX data model.

These files are **not** part of ripbi's source code and are not covered by the
Apache-2.0/MIT dual license at the repository root. They are copied or exported
from <https://github.com/microsoft/powerbi-desktop-samples> and redistributed
here under the MIT license reproduced below, as that license requires.

Local Power BI Desktop artifacts (`*/.pbi/` directories) are regenerated when a
project is opened and are intentionally not committed.

---

MIT License

Copyright (c) Microsoft Corporation. All rights reserved.

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
