<script>
  import { onMount } from 'svelte';
  import { basicSetup, EditorView } from 'codemirror';
  import { Compartment } from '@codemirror/state';
  import { keymap } from '@codemirror/view';
  import { sql, SQLite } from '@codemirror/lang-sql';
  import { oneDark } from '@codemirror/theme-one-dark';

  export let value = '';
  export let schema = {};
  export let onrun = () => {};
  let host;
  let editor;
  const language = new Compartment();

  onMount(() => {
    editor = new EditorView({
      parent: host,
      doc: value,
      extensions: [
        keymap.of([{ key: 'Mod-Enter', run: () => { onrun(); return true; } }]),
        basicSetup, oneDark,
        language.of(sql({ dialect: SQLite, schema, upperCaseKeywords: true })),
        EditorView.contentAttributes.of({ 'aria-label': 'SQL query' }),
        EditorView.lineWrapping,
        EditorView.theme({ '&': { height: '100%' }, '.cm-scroller': { overflow: 'auto', minHeight: '140px', maxHeight: '320px' } }),
        EditorView.updateListener.of(update => { if (update.docChanged) value = update.state.doc.toString(); }),
      ],
    });
    return () => editor.destroy();
  });

  $: if (editor && value !== editor.state.doc.toString()) {
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: value } });
  }
  $: if (editor) editor.dispatch({ effects: language.reconfigure(sql({ dialect: SQLite, schema, upperCaseKeywords: true })) });
</script>

<div class="sql-editor" bind:this={host}></div>

<style>
  .sql-editor { min-height: 140px; flex-shrink: 0; border: 1px solid var(--border); border-radius: 7px; overflow: hidden; font-size: 14px; }
</style>
