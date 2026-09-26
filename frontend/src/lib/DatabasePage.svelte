<script>
  import { onMount } from 'svelte';
  import SqlEditor from './SqlEditor.svelte';
  import { browseQuery, databaseRequest, displayCell, identityColumns, mutationQuery, quoteIdentifier, resultCsv, rowValues, visibleColumns } from './database.js';

  let tables = [];
  let selected = '';
  let tableFilter = '';
  let search = '';
  let sort = '';
  let descending = false;
  let offset = 0;
  let pageSize = 50;
  let previousOffsets = [];
  let rows = [];
  let hasMore = false;
  let view = 'browse';
  let busy = false;
  let error = '';
  let notice = '';
  let writes = false;
  let sql = 'SELECT name, type, sql\nFROM sqlite_schema\nORDER BY name;';
  let result = null;
  let editing = null;
  let drafts = [];
  let confirmAction = null;

  $: table = tables.find(item => item.name === selected);
  $: columns = table ? visibleColumns(table) : [];
  $: schema = Object.fromEntries(tables.map(item => [item.name, visibleColumns(item).map(c => c.name)]));
  $: filteredTables = tables.filter(item => item.name.toLowerCase().includes(tableFilter.toLowerCase()));
  $: canEdit = writes && table?.kind === 'table' && identityColumns(table).length > 0;

  onMount(() => perform(refresh));

  async function perform(action) {
    if (busy) return;
    busy = true;
    error = '';
    notice = '';
    try { await action(); }
    catch (e) { error = e.message; }
    finally { busy = false; }
  }

  async function refresh() {
    const data = await databaseRequest('schema');
    tables = data.tables;
    if (!tables.some(item => item.name === selected)) selected = tables[0]?.name || '';
    if (selected) await loadRows();
    else rows = [];
  }

  async function loadRows(reset = false) {
    if (reset) { offset = 0; previousOffsets = []; }
    const current = tables.find(item => item.name === selected);
    if (!current) return;
    rows = [];
    hasMore = false;
    const data = await databaseRequest('query', browseQuery(current, { search, sort, descending, offset, pageSize }));
    hasMore = data.rows.length > pageSize || data.truncated;
    rows = data.rows.slice(0, pageSize);
  }

  function chooseTable(name) {
    if (busy) return;
    selected = name;
    search = '';
    sort = '';
    descending = false;
    view = 'browse';
    perform(() => loadRows(true));
  }

  function sortBy(name) {
    descending = sort === name ? !descending : false;
    sort = name;
    perform(() => loadRows(true));
  }

  function paginate(next) {
    if (next) { previousOffsets = [...previousOffsets, offset]; offset += rows.length; }
    else { offset = previousOffsets.at(-1) ?? 0; previousOffsets = previousOffsets.slice(0, -1); }
    perform(() => loadRows());
  }

  function runSql() {
    if (busy || !sql.trim()) return;
    const query = sql;
    if (writes) {
      confirmAction = {
        title: 'Run SQL with writes enabled?',
        description: 'This statement can change the live Nagare database. Changes commit immediately.',
        sql: query,
        run: () => executeSql(query, true),
      };
    } else perform(() => executeSql(query, false));
  }

  async function executeSql(query, allowWrite) {
    result = null;
    result = await databaseRequest('query', { sql: query, allow_write: allowWrite });
    notice = result.readonly ? `Query completed in ${result.elapsed_ms} ms.` : `Statement committed. ${result.changes} row(s) affected.`;
    if (!result.readonly) await refresh();
  }

  function openEditor(row = null) {
    editing = { table, row };
    drafts = row ? rowValues(table, row).map(cell => ({ ...cell }))
      : columns.map(() => ({ type: 'default', value: '' }));
    error = '';
  }

  function saveRow() {
    try {
      const action = editing.row ? 'update' : 'insert';
      const query = mutationQuery(editing.table, editing.row, drafts, action);
      confirmAction = {
        title: action === 'insert' ? 'Insert this row?' : 'Save these changes?',
        description: `Changes to ${editing.table.name} commit immediately.`,
        sql: query.sql,
        run: async () => {
          await databaseRequest('query', { ...query, allow_write: true });
          editing = null;
          notice = action === 'insert' ? 'Row inserted.' : 'Row updated.';
          await loadRows(action === 'insert');
        },
      };
    } catch (e) { error = e.message; }
  }

  function deleteRow(row) {
    try {
      const query = mutationQuery(table, row, [], 'delete');
      confirmAction = {
        title: 'Delete this row?',
        description: `This permanently deletes the selected row from ${table.name}. Related rows may also be deleted by foreign-key rules.`,
        sql: query.sql,
        run: async () => {
          await databaseRequest('query', { ...query, allow_write: true });
          notice = 'Row deleted.';
          await loadRows();
          if (!rows.length && offset > 0) {
            offset = previousOffsets.at(-1) ?? 0;
            previousOffsets = previousOffsets.slice(0, -1);
            await loadRows();
          }
        },
      };
    } catch (e) { error = e.message; }
  }

  function confirm() {
    const action = confirmAction;
    confirmAction = null;
    perform(action.run);
  }

  function exportCsv() {
    const exportedColumns = view === 'query' ? result?.columns : columns.map(c => c.name);
    const exportedRows = view === 'query' ? result?.rows : rows.map(row => rowValues(table, row));
    if (!exportedColumns) return;
    const url = URL.createObjectURL(new Blob([resultCsv(exportedColumns, exportedRows)], { type: 'text/csv;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = view === 'query' ? 'nagare-query.csv' : `${selected}.csv`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  // Native dialogs provide focus trapping, Escape handling and focus restoration.
  function showDialog(node) { node.showModal(); }
</script>

<svelte:head><title>Database · Nagare</title></svelte:head>

<div class="database-page">
  <header>
    <div><a class="back" href="/">← Nagare</a><h1>Database <span>Advanced</span></h1></div>
    <label class="write-toggle"><input type="checkbox" bind:checked={writes} disabled={busy || !!editing} /> Enable writes</label>
  </header>
  <p class="database-notice" class:write-warning={writes}>
    <strong>nagare.sqlite</strong> · {writes ? 'Writes enabled. Saved changes affect the live database immediately.' : 'Read-only mode. Enable writes to insert, edit, delete, or run modifying SQL.'}
    <span>Configuration and active history are cached; direct edits may require a Nagare restart and may be overwritten while it is running.</span>
  </p>

  <div class="workspace">
    <aside aria-label="Database tables">
      <div class="sidebar-title"><h2>Tables & views</h2><button class="compact" disabled={busy} on:click={() => perform(refresh)} aria-label="Refresh database">↻</button></div>
      <input class="table-search" aria-label="Find a table" placeholder="Find a table…" bind:value={tableFilter} />
      <div class="table-list">
        {#each filteredTables as item}
          <button class:chosen={selected === item.name} disabled={busy} on:click={() => chooseTable(item.name)} title={item.name}>
            <span>{item.name}</span><small>{item.kind === 'table' ? item.columns.length : item.kind}</small>
          </button>
        {/each}
        {#if !tables.length && !busy}<p class="muted">No tables found.</p>{/if}
      </div>
    </aside>

    <main>
      <nav aria-label="Database views">
        <button class:active={view === 'browse'} disabled={busy} on:click={() => view = 'browse'}>Browse data</button>
        <button class:active={view === 'query'} disabled={busy} on:click={() => view = 'query'}>SQL query</button>
        <button class:active={view === 'schema'} disabled={busy} on:click={() => view = 'schema'}>Schema</button>
      </nav>
      {#if error}<p class="message error" role="alert">{error}</p>{/if}
      {#if notice}<p class="message success" role="status">{notice}</p>{/if}
      {#if busy}<p class="loading" role="status">Working…</p>{/if}

      {#if view === 'browse' && table}
        <div class="toolbar">
          <h2>{table.name}</h2>
          <button disabled={busy || !writes || table.kind !== 'table'} on:click={() => openEditor()}>+ Insert row</button>
          <button disabled={busy || !rows.length} on:click={exportCsv}>Export page CSV</button>
        </div>
        <form class="filter-bar" on:submit|preventDefault={() => perform(() => loadRows(true))}>
          <input aria-label="Search table values" placeholder="Search values in all columns…" bind:value={search} disabled={busy} />
          <button type="submit" disabled={busy}>Filter</button>
          <button type="button" disabled={busy} on:click={() => { search = ''; perform(() => loadRows(true)); }}>Clear</button>
          <select aria-label="Rows per page" bind:value={pageSize} disabled={busy} on:change={() => perform(() => loadRows(true))}>
            <option value={25}>25 rows</option><option value={50}>50 rows</option><option value={100}>100 rows</option>
          </select>
        </form>
        <div class="data-grid">
          <table>
            <thead><tr><th class="row-actions">Row</th>{#each columns as column}<th><button disabled={busy} on:click={() => sortBy(column.name)}>{column.name}{sort === column.name ? (descending ? ' ↓' : ' ↑') : ''}<small>{column.data_type || 'ANY'}{column.pk ? ' · PK' : ''}</small></button></th>{/each}</tr></thead>
            <tbody>
              {#each rows as row, index}
                <tr><td class="row-actions"><span>{offset + index + 1}</span><button class="compact" disabled={busy || !canEdit} on:click={() => openEditor(row)} aria-label={`Edit row ${offset + index + 1}`}>Edit</button><button class="compact delete" disabled={busy || !canEdit} on:click={() => deleteRow(row)} aria-label={`Delete row ${offset + index + 1}`}>Delete</button></td>
                  {#each rowValues(table, row) as cell}<td class:null-cell={cell.type === 'null'} title={displayCell(cell)}><div class="cell">{displayCell(cell)}</div></td>{/each}
                </tr>
              {/each}
            </tbody>
          </table>
          {#if !rows.length && !busy}<p class="empty">No rows{search ? ' match this filter' : ''}.</p>{/if}
        </div>
        <footer><span>{rows.length ? `Rows ${offset + 1}–${offset + rows.length}` : '0 rows'} · previews marked when large</span><div><button disabled={busy || !previousOffsets.length} on:click={() => paginate(false)}>Previous</button><button disabled={busy || !hasMore || !rows.length} on:click={() => paginate(true)}>Next</button></div></footer>
        {#if table.kind !== 'table' || !identityColumns(table).length}<p class="muted">This object has no editable row identity. Browse it here or use the SQL editor.</p>{/if}
      {:else if view === 'query'}
        <div class="toolbar"><h2>SQL query</h2><button disabled={busy || !table} on:click={() => sql = `SELECT * FROM "main".${quoteIdentifier(table.name)} LIMIT 100;`}>Select current table</button></div>
        <SqlEditor bind:value={sql} {schema} onrun={runSql} />
        <div class="query-actions"><button class="primary" disabled={busy || !sql.trim()} on:click={runSql}>Run query</button><span class="muted">Ctrl / ⌘ Enter · one statement · 1,000 rows maximum · 5s execution limit</span></div>
        {#if result}
          <div class="toolbar"><span>{result.rows.length} result row(s){result.truncated ? ' · result limited; use LIMIT / OFFSET to continue' : ''}</span><button disabled={!result.columns.length} on:click={exportCsv}>Export result CSV</button></div>
          {#if result.columns.length}<div class="data-grid"><table><thead><tr>{#each result.columns as name}<th>{name}</th>{/each}</tr></thead><tbody>{#each result.rows as row}<tr>{#each row as cell}<td class:null-cell={cell.type === 'null'} title={displayCell(cell)}><div class="cell">{displayCell(cell)}</div></td>{/each}</tr>{/each}</tbody></table></div>{/if}
        {/if}
      {:else if view === 'schema' && table}
        <div class="toolbar"><h2>{table.name}</h2><span class="muted">{table.kind}</span></div>
        <pre>{table.sql || 'No CREATE statement available.'}</pre>
        <div class="data-grid"><table><thead><tr><th>Column</th><th>Type</th><th>Nullable</th><th>Default</th><th>Key / generated</th></tr></thead><tbody>{#each table.columns as column}<tr><td>{column.name}</td><td>{column.data_type || 'ANY'}</td><td>{column.not_null ? 'No' : 'Yes'}</td><td>{column.default_value ?? '—'}</td><td>{column.pk ? `Primary key ${column.pk}` : ''}{column.hidden ? ' Generated / hidden' : ''}</td></tr>{/each}</tbody></table></div>
      {/if}
    </main>
  </div>
</div>

{#if editing}
  <dialog class="row-dialog" use:showDialog on:cancel|preventDefault={() => { if (!busy) editing = null; }}>
    <form on:submit|preventDefault={saveRow}>
      <div class="dialog-heading"><h2>{editing.row ? 'Edit row' : 'Insert row'} · {editing.table.name}</h2><button type="button" disabled={busy} on:click={() => editing = null} aria-label="Close row editor">✕</button></div>
      <p class="muted">Choose a SQLite type for each value. NULL, empty text, and a column default are different values. BLOB values use hexadecimal.</p>
      {#if error}<p class="message error" role="alert">{error}</p>{/if}
      <div class="fields">
        {#each visibleColumns(editing.table) as column, i}
          <div class="field">
            <label for={`value-${i}`}>{column.name}<small>{column.data_type || 'ANY'}{column.pk ? ' · PK' : ''}{column.not_null ? ' · required' : ''}</small></label>
            <select aria-label={`Type for ${column.name}`} bind:value={drafts[i].type} disabled={busy || !!column.hidden || drafts[i].truncated}>
              {#if !editing.row}<option value="default">Default / omitted</option>{/if}
              <option value="null">NULL</option><option value="text">Text</option><option value="integer">Integer</option><option value="real">Real</option><option value="blob">BLOB (hex)</option>
            </select>
            <textarea id={`value-${i}`} rows={drafts[i].value.length > 100 || drafts[i].value.includes('\n') ? 4 : 2} bind:value={drafts[i].value} disabled={busy || !!column.hidden || drafts[i].truncated || ['null', 'default'].includes(drafts[i].type)}></textarea>
            {#if drafts[i].truncated}<small class="muted">Preview only; use SQL to change this large value.</small>{/if}
            {#if column.default_value !== null && !editing.row}<small class="muted">Default: {column.default_value}</small>{/if}
          </div>
        {/each}
      </div>
      <div class="dialog-actions"><button type="button" disabled={busy} on:click={() => editing = null}>Cancel</button><button class="primary" type="submit" disabled={busy || !writes}>{editing.row ? 'Save row' : 'Insert row'}</button></div>
    </form>
  </dialog>
{/if}

{#if confirmAction}
  <dialog class="confirm-dialog" use:showDialog on:cancel|preventDefault={() => confirmAction = null}>
    <h2>{confirmAction.title}</h2><p>{confirmAction.description}</p><pre>{confirmAction.sql}</pre>
    <div class="dialog-actions"><button on:click={() => confirmAction = null}>Cancel</button><button class="primary" on:click={confirm}>Confirm</button></div>
  </dialog>
{/if}

<style>
  :global(#app:has(.database-page)) { width: 100%; text-align: left; }
  .database-page { height: 100%; display: flex; flex-direction: column; padding: 24px; gap: 18px; }
  header, .toolbar, .sidebar-title, footer, .dialog-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  h1 { font-size: 24px; margin: 8px 0 0; letter-spacing: 0; color: var(--text-primary); } h1 span { font-size: 11px; color: var(--text-secondary); border: 1px solid var(--border); padding: 4px 8px; border-radius: 20px; vertical-align: middle; font-weight: 400; }
  h2 { font-size: 15px; margin: 0; overflow-wrap: anywhere; color: var(--text-primary); } .back { color: var(--text-secondary); font-size: 13px; text-decoration: none; }
  .write-toggle { display: flex; align-items: center; gap: 8px; font-size: 13px; white-space: nowrap; } input[type=checkbox] { accent-color: var(--accent); }
  .database-notice { font-size: 12px; line-height: 1.7; color: var(--text-secondary); padding: 12px 16px; background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 8px; }
  .database-notice span { display: block; } .write-warning { border-color: var(--warning); }
  .workspace { flex: 1; min-height: 0; display: grid; grid-template-columns: 235px minmax(0, 1fr); gap: 20px; }
  aside { min-height: 0; display: flex; flex-direction: column; gap: 12px; border-right: 1px solid var(--border); padding-right: 18px; }
  .table-search { width: 100%; font-size: 13px; } .table-list { overflow: auto; }
  .table-list button { width: 100%; display: flex; justify-content: space-between; gap: 8px; text-align: left; background: transparent; border-color: transparent; padding: 10px 8px; margin-bottom: 3px; font-size: 12px; }
  .table-list button span { overflow: hidden; text-overflow: ellipsis; } .table-list button.chosen { background: var(--bg-card); border-color: var(--border); color: var(--accent); }
  small, .muted { color: var(--text-secondary); font-size: 12px; } main { min-width: 0; overflow: auto; display: flex; flex-direction: column; gap: 14px; }
  main > :global(*) { flex-shrink: 0; }
  nav { display: flex; border-bottom: 1px solid var(--border); gap: 6px; } nav button { background: transparent; border: none; border-radius: 0; border-bottom: 2px solid transparent; font-size: 13px; }
  nav button.active { color: var(--accent); border-bottom-color: var(--accent); }
  .toolbar { flex-wrap: wrap; font-size: 12px; } .toolbar h2 { margin-right: auto; } button { font-size: 12px; } button:disabled { opacity: 0.4; cursor: not-allowed; }
  .compact { padding: 4px 7px; font-size: 11px; } .delete { color: var(--accent); }
  .filter-bar, .query-actions { display: flex; align-items: center; gap: 8px; } .filter-bar input { flex: 1; min-width: 100px; font-size: 13px; } .filter-bar select { font-size: 12px; }
  .data-grid { overflow: auto; min-height: 80px; max-height: 65vh; border: 1px solid var(--border); border-radius: 7px; }
  table { border-collapse: separate; border-spacing: 0; font-size: 12px; width: 100%; text-align: left; }
  th, td { padding: 10px 12px; border-right: 1px solid var(--border); border-bottom: 1px solid var(--border); vertical-align: top; }
  th { position: sticky; top: 0; background: var(--bg-secondary); z-index: 1; white-space: nowrap; font-weight: 500; }
  th button { padding: 0; background: transparent; border: none; text-align: left; } th small { display: block; font-size: 10px; margin-top: 4px; font-weight: 400; }
  tr:hover td { background: var(--bg-secondary); } .cell { min-width: 90px; max-width: 340px; max-height: 72px; overflow: hidden; white-space: pre-wrap; overflow-wrap: anywhere; font-family: Consolas, monospace; line-height: 1.5; }
  .null-cell { color: var(--text-dim); font-style: italic; } .row-actions { white-space: nowrap; width: 145px; } .row-actions span { display: inline-block; min-width: 24px; color: var(--text-secondary); margin-right: 6px; } .row-actions button + button { margin-left: 4px; }
  .empty { padding: 32px; color: var(--text-secondary); text-align: center; } footer { font-size: 12px; color: var(--text-secondary); } footer button + button { margin-left: 6px; }
  .message { padding: 10px 12px; border-radius: 6px; font-size: 13px; overflow-wrap: anywhere; } .error { color: #ffb0b6; background: #431a29; } .success { color: var(--success); background: #132c23; } .loading { font-size: 12px; color: var(--text-secondary); }
  pre { overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg-primary); border: 1px solid var(--border); padding: 16px; border-radius: 7px; font-size: 12px; line-height: 1.6; }
  dialog { color: var(--text-primary); background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 12px; padding: 24px; margin: auto; width: min(720px, calc(100vw - 32px)); max-height: 85vh; overflow: auto; }
  dialog::backdrop { background: #000a; } dialog h2 { font-size: 18px; } dialog p { margin: 16px 0; font-size: 13px; line-height: 1.6; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 20px; } .fields { display: grid; gap: 16px; } .field { display: grid; grid-template-columns: minmax(0, 1fr) 145px; gap: 8px; }
  .field label { font-size: 13px; overflow-wrap: anywhere; } .field label small { display: block; margin-top: 4px; } .field textarea { grid-column: 1 / -1; font-family: Consolas, monospace; font-size: 13px; resize: vertical; min-height: 48px; } .field select { font-size: 12px; } .field > small { grid-column: 1 / -1; }
  @media (max-width: 800px) {
    .database-page { padding: 12px; overflow: auto; } .workspace { grid-template-columns: 1fr; overflow: visible; min-height: auto; } aside { border-right: none; padding: 0; } .table-list { display: flex; max-height: 120px; flex-wrap: wrap; } .table-list button { width: auto; } main { overflow: visible; } .filter-bar, .query-actions { flex-wrap: wrap; } .data-grid { max-height: 55vh; } footer { flex-wrap: wrap; }
  }
</style>
