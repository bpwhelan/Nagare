import test from 'node:test';
import assert from 'node:assert/strict';
import { browseQuery, mutationQuery, resultCsv } from './database.js';

const cell = (type, value = '') => ({ type, value });
const table = {
  name: 'notes"; DROP TABLE notes; --', kind: 'table', rowid: '_rowid_',
  columns: [{ name: 'id', hidden: 0, pk: 1 }, { name: 'body', hidden: 0, pk: 0 }],
};

test('browsing quotes identifiers and binds search text including LIKE wildcards', () => {
  const query = browseQuery(table, { search: "日本語_%'", sort: 'body; DELETE', offset: 50 });
  assert.ok(query.sql.includes('"notes""; DROP TABLE notes; --"'));
  assert.ok(query.sql.includes('ORDER BY "_rowid_" ASC'));
  assert.ok(!query.sql.includes('日本語'));
  assert.equal(query.params[0].value, "%日本語\\_\\%'%");
  assert.equal(query.params.at(-1).value, '50');
});

test('an edit preserves large row IDs, binds empty strings, and compares the original value', () => {
  const row = [cell('integer', '9223372036854775807'), cell('integer', '10'), cell('text', 'old')];
  const query = mutationQuery(table, row, [row[1], cell('text', '')]);
  assert.ok(query.sql.endsWith('WHERE "_rowid_" IS ? COLLATE BINARY AND "body" IS ? COLLATE BINARY'));
  assert.deepEqual(query.params, [cell('text', ''), row[0], row[2]]);
  assert.equal(query.expected_changes, 1);
});

test('deleting a WITHOUT ROWID row uses the entire composite key and original snapshot', () => {
  const composite = { name: 'known', kind: 'table', rowid: null, columns: [{ name: 'scope', pk: 2, hidden: 0 }, { name: 'term', pk: 1, hidden: 0 }] };
  const row = [cell('text', 'jp'), cell('text', '流れ')];
  const query = mutationQuery(composite, row, [], 'delete');
  assert.deepEqual(query.params.slice(0, 2), [row[1], row[0]]);
  assert.equal(query.expected_changes, 1);
  assert.throws(() => mutationQuery({ ...composite, kind: 'view' }, row, [], 'delete'), /no usable row identity/);
});

test('insert distinguishes defaults, NULL, empty strings and generated columns', () => {
  const target = { ...table, columns: [...table.columns, { name: 'generated', hidden: 3 }] };
  const query = mutationQuery(target, null, [cell('default'), cell('null'), cell('text', 'ignored')], 'insert');
  assert.ok(query.sql.includes('("body") VALUES (?)'));
  assert.deepEqual(query.params, [cell('null')]);
  assert.match(mutationQuery(table, null, [cell('default'), cell('default')], 'insert').sql, /DEFAULT VALUES$/);
});

test('truncated values cannot be used to overwrite a cell or identify a row', () => {
  const row = [cell('integer', '1'), cell('integer', '1'), { ...cell('text', 'preview'), truncated: true }];
  assert.throws(() => mutationQuery(table, row, [row[1], cell('text', 'new')]), /preview cannot be overwritten/);
  const composite = { ...table, rowid: null };
  assert.throws(() => mutationQuery(composite, [{ ...row[1], truncated: true }, row[2]], [], 'delete'), /preview in its key/);
});

test('CSV quotes newlines, commas and quotes, and identifies previews', () => {
  assert.equal(resultCsv(['body'], [[cell('text', 'a,"b"\nc')], [{ ...cell('text', 'partial'), truncated: true }]]), '"body"\r\n"a,""b""\nc"\r\n"partial… [preview]"');
});
