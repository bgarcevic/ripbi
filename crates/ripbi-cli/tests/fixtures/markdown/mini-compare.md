### ripbi scan: `Mini.SemanticModel`

**1 new finding** · 0 fixed · 2 already in `../base`

| Finding | Count |
|---|--:|
| Unused measures | 1 |

#### New findings (1)

| Type | Object |
|---|---|
| measure | `'Sales'[Draft KPI]` |

> Remove the new unused objects, or if something outside these reports reads one (an Excel pivot, another workspace's report), mark it in the model: `annotation ripbi_keep = <reason>`

#### Worst tables

| Table | Unused |
|---|--:|
| `'Sales'` | 1 |

