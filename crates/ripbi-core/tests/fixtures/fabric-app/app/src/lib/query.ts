import { spec } from '../config';

const col = (c: string) => `'${spec.table.replace(/'/g, "''")}'[${c}]`;

/* EVALUATE 'Ledger'[Quarter] */
export const revenueQuery = (c: string) => `EVALUATE
SUMMARIZECOLUMNS(${col(c)}, "Revenue", [Revenue])`;
