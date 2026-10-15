# Cleaning up a model

A first scan of a long-lived model often lists hundreds of findings. This is one
way to work through them. The examples use the committed AdventureWorks sample;
run them from the root of a clone of this repository.

## 1. Get the overview

```sh
ripbi scan "samples/AdventureWorks Sales.pbip" --summary
```

`--summary` prints counts per object type and the tables with the most
findings, so you can start where the most cleanup is. Add `--sort size` to a
PBIX or `.abf` scan to list the findings that cost the most memory first (see
[Storage sizes](scan/storage.md)).

## 2. Work through one type at a time

```sh
ripbi scan "samples/AdventureWorks Sales.pbip" --type measure
```

Each finding says why nothing keeps it alive. `← only used by … (also unused)`
means the whole chain can go together. [What counts as unused](unused.md)
explains the rules.

## 3. Check what the scan cannot see

Before you delete anything, ask whether something outside these reports uses
the object: a thin report in another workspace, an Excel pivot, an XMLA client.
To see everything that depends on an object and everything it depends on:

```sh
ripbi deps "'Sales'[Average Sales per Order]" --model "samples/AdventureWorks Sales.SemanticModel" --impact
```

A [workspace-monitoring query log](scan/queries.md) shows what other clients
actually queried; pass it with `--queries-from` and those objects stop being
findings.

## 4. Keep what is unused on purpose

Mark an object that a consumer ripbi cannot see reads with a `ripbi_keep`
annotation in the model, with the reason as its value (see
[Keeping objects on purpose](scan/keep.md)). For whole families of objects ripbi
should not judge, add an `ignore` pattern to [`ripbi.toml`](config.md).

## 5. Remove the rest

Delete the objects in Power BI Desktop, Tabular Editor, or the TMDL files. With
[tomix](https://github.com/bgarcevic/tomix-cli), remove the whole list as one
change (see [Removing with tomix](scan/tx-rm.md)):

```sh
ripbi scan --emit tx-rm | tx rm --from - ./Model.SemanticModel --save
```

Then scan again: removing objects can make others unused, such as a key column
whose only relationship is gone.

## 6. Keep it clean

Add the scan to CI so that a pull request fails only when it adds a finding; old
findings never block it. See [CI in five minutes](ci.md).
