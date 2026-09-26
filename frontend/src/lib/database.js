export async function databaseRequest(path, body) {
  const response = await fetch(`/api/advanced/database/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Nagare-Database': '1' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || `Request failed (${response.status})`);
  return result;
}

export const quoteIdentifier = name => `"${name.replaceAll('"', '""')}"`;
const tableName = table => `"main".${quoteIdentifier(table.name)}`;
export const visibleColumns = table => table.columns.filter(column => column.hidden !== 1);
export const rowValues = (table, row) => table.rowid ? row.slice(1) : row;

export function identityColumns(table) {
  if (table.kind !== 'table') return [];
  return table.rowid ? [table.rowid] : table.columns.filter(c => c.pk > 0).sort((a, b) => a.pk - b.pk).map(c => c.name);
}

function identity(table, row) {
  const names = identityColumns(table);
  if (!names.length) throw new Error('This table has no usable row identity. Use SQL to edit it.');
  const columns = visibleColumns(table);
  const values = rowValues(table, row);
  const params = table.rowid ? [row[0]] : names.map(name => values[columns.findIndex(c => c.name === name)]);
  if (params.some(cell => !cell || cell.truncated)) throw new Error('This row has a preview in its key. Use SQL to edit it.');
  return { conditions: names.map(name => `${quoteIdentifier(name)} IS ? COLLATE BINARY`), params };
}

export function browseQuery(table, { search = '', sort = '', descending = false, offset = 0, pageSize = 50 } = {}) {
  const columns = visibleColumns(table);
  const select = [...(table.rowid ? [table.rowid] : []), ...columns.map(c => c.name)].map(quoteIdentifier).join(', ');
  const params = [];
  let sql = `SELECT ${select} FROM ${tableName(table)}`;
  if (search) {
    const pattern = `%${search.replace(/[\\%_]/g, '\\$&')}%`;
    sql += ` WHERE ${columns.map(c => {
      params.push({ type: 'text', value: pattern });
      return `CAST(${quoteIdentifier(c.name)} AS TEXT) LIKE ? ESCAPE '\\'`;
    }).join(' OR ')}`;
  }
  const order = columns.some(c => c.name === sort) ? [sort] : identityColumns(table);
  if (order.length) sql += ` ORDER BY ${order.map(quoteIdentifier).join(', ')} ${descending ? 'DESC' : 'ASC'}`;
  sql += ' LIMIT ? OFFSET ?';
  params.push({ type: 'integer', value: String(pageSize + 1) }, { type: 'integer', value: String(offset) });
  return { sql, params };
}

export function mutationQuery(table, originalRow, drafts, action = 'update') {
  const columns = visibleColumns(table);
  const original = originalRow ? rowValues(table, originalRow) : [];
  if (action === 'delete') {
    const key = identity(table, originalRow);
    columns.forEach((column, i) => {
      if (!original[i].truncated) {
        key.conditions.push(`${quoteIdentifier(column.name)} IS ? COLLATE BINARY`);
        key.params.push(original[i]);
      }
    });
    return { sql: `DELETE FROM ${tableName(table)} WHERE ${key.conditions.join(' AND ')}`, params: key.params, expected_changes: 1 };
  }

  const changed = columns.map((column, i) => ({ column, cell: drafts[i], original: original[i] }))
    .filter(({ column, cell, original }) => column.hidden === 0 && cell.type !== 'default' && !cell.truncated
      && (!original || cell.type !== original.type || cell.value !== original.value));
  if (action === 'insert') {
    return {
      sql: changed.length ? `INSERT INTO ${tableName(table)} (${changed.map(({ column }) => quoteIdentifier(column.name)).join(', ')}) VALUES (${changed.map(() => '?').join(', ')})`
        : `INSERT INTO ${tableName(table)} DEFAULT VALUES`,
      params: changed.map(({ cell }) => cell), expected_changes: 1,
    };
  }
  if (!changed.length) throw new Error('No values have changed.');
  const key = identity(table, originalRow);
  changed.forEach(({ column, original }) => {
    if (original.truncated) throw new Error('A preview cannot be overwritten. Use SQL to edit this cell.');
    key.conditions.push(`${quoteIdentifier(column.name)} IS ? COLLATE BINARY`);
    key.params.push(original);
  });
  return {
    sql: `UPDATE ${tableName(table)} SET ${changed.map(({ column }) => `${quoteIdentifier(column.name)} = ?`).join(', ')} WHERE ${key.conditions.join(' AND ')}`,
    params: [...changed.map(({ cell }) => cell), ...key.params], expected_changes: 1,
  };
}

export function displayCell(cell) {
  if (cell.type === 'null') return 'NULL';
  if (cell.type === 'blob') return `BLOB · ${cell.bytes ?? cell.value.length / 2} bytes`;
  if (cell.type === 'text' && cell.value === '') return '(empty text)';
  return cell.value + (cell.truncated ? '… [preview]' : '');
}

export function resultCsv(columns, rows) {
  const escape = value => `"${String(value).replaceAll('"', '""')}"`;
  return [columns, ...rows.map(row => row.map(cell => cell.type === 'null' ? '' : cell.value + (cell.truncated ? '… [preview]' : '')))]
    .map(row => row.map(escape).join(',')).join('\r\n');
}
