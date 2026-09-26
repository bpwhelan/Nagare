# Database maintenance

Open **Settings → Server**, scroll to the bottom, expand **Advanced maintenance**,
and choose **Open database viewer**. The viewer opens in a separate tab at
`/database.html`; it is not part of the main navigation.

The viewer works on the server's live `nagare.sqlite` file. Browse tables and views,
filter values, sort columns, inspect schema, or export the current page/results to CSV.
The SQL editor uses CodeMirror with SQLite highlighting and table/column completion.
Run a query with **Ctrl+Enter** (or **⌘+Enter**).

Reads are the default. **Enable writes** allows row insertion, editing, deletion,
and modifying SQL. Each change requires confirmation and commits immediately.
Row forms distinguish NULL, empty text, omitted/default values, integers, reals,
and hexadecimal BLOBs. Generated columns and large value previews are not editable
in the row form. Use SQL for those values. Existing row edits compare the original
values, and a stale or ambiguous edit is rolled back instead of updating another row.

Run one SQL statement at a time. Results stop at 1,000 rows or the response size
limit, and long-running SQL is interrupted after five seconds. Use LIMIT/OFFSET
for larger reads. Failed writes roll back, including writes with oversized RETURNING
results. Foreign keys remain enabled. Database attachment, filesystem operations,
manual transaction control, and settings-changing PRAGMAs are unavailable.

Direct database changes bypass Nagare's application caches. Configuration and active
history edits may require a server restart and can be overwritten by running services.
Make a database backup before repairs. Like the rest of Nagare, this tool relies on
the server's existing access controls; keep Nagare on a trusted network or behind
your authenticated proxy. The SQL API accepts same-origin browser requests only.
