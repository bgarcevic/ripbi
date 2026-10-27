// Queries composed at run time against the Books model.
const scope = (year: number) => `TREATAS({${year}}, 'Ledger'[Year])`;

// "Total" is introduced before the interpolation and read after it.
export const byAccount = (year: number) =>
  `EVALUATE FILTER(SUMMARIZECOLUMNS('Ledger'[Account], "Total", [Revenue], ${scope(year)}), [Total] > 0 && 'Ledger'[Account] <> "") ORDER BY [Total] DESC`;

export const byRegion = (year: number) =>
  `EVALUATE SUMMARIZECOLUMNS(${scope(year)}, 'Ledger'[Region], "revenue", [Revenue], "gross", [Margin], "stale", [Stale Margin])`;

export const plan = `EVALUATE ROW("plan", SUM('Budget'[Amount]), "actual", [Revenue])`;

// Every reference is gone: nothing anchors the literal to the model.
export const retired = `EVALUATE ROW("x", [Retired Measure])`;
