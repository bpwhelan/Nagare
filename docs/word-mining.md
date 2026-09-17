# Word mining from History

Open **History → Watch history → Mine words** on any file. Nagare uses that
file's media and saved subtitles. You can also choose a matching SRT, VTT, ASS,
or SSA file in the mining workspace. Uploaded subtitles belong to this workspace;
they do not replace the subtitles used for live playback.

Click **Analyze saved subtitles** to discover Japanese vocabulary. Analysis uses
the native [Sudachi.rs](https://github.com/WorksApplications/sudachi.rs) tokenizer
with Sudachi Core. Mode B is the default; A splits into shorter units and C keeps
longer compounds. Inflected words are grouped under their dictionary forms.
English meanings come from JMdict. A search can also add a literal phrase from
the subtitles when the tokenizer does not produce the expression you want.

## Choose cards

- Search words, readings, surface forms, or meanings; filter by common words,
  repetition, known status, or i+1 context.
- Mark words known, ignore them, paste a known-word list, or sync a word field from
  an existing Anki note type. Anki words count as familiar for recommendations;
  this does not mean Nagare has assessed whether you have mastered them. Ignored
  words are hidden from new-word suggestions but still count as unfamiliar.
- Select recommended words, select the filtered list, or pick words individually.
  Suggestions favor common, repeated words with dictionary meanings and fewer
  unfamiliar content words. These are heuristics, not a proficiency assessment.
- Choose a subtitle occurrence, add adjacent lines, edit its sentence, reading,
  meaning, and clip timing, then preview audio or a frame. Clips may be up to
  90 seconds. Your choices are saved in this browser for this subtitle revision.

## Send to Anki

Open **Anki & audio settings**, choose a deck, and select a note type. The built-in
**Nagare Vocabulary** note type is created when its first batch starts. Existing
note types support custom field mappings; word and sentence are required, while
reading, meaning, audio, image, and source may be omitted. Sentence audio follows
Nagare's configured codec. Screenshots can be still images or animated AVIFs.
Audio-only files omit screenshots automatically.

The media's target-language audio track is selected automatically. If several
tracks exist and none matches, choose a track before creating the batch. The
choice is saved with the batch and its review session.

Use **Mine this word** or **Review N cards**, inspect the selection and deck, and
click **Create N cards**. Up to 500 distinct words can be mined in one batch.
Words already present in the chosen note type are skipped, across all decks.
Known-word sync and duplicate checks compare word-field text exactly after
removing HTML/ruby markup and bracketed readings; they do not infer synonyms.

Progress is saved on the server and continues with the browser closed. Pause
finishes the current card. Resume retries unfinished cards, first checking Anki
for a saved operation tag so a lost response does not create a duplicate. After
a Nagare restart, interrupted batches remain paused until resumed. Keep the
generated `nagare::word_miner` tags while recovering a batch. Previous batches
are available at the bottom of the file's workspace.

Created cards appear in **History → Card sessions** and in the batch's **Review
cards** link, with their original subtitle snapshot and timing. Later analyses
do not alter existing review sessions. Each batch uses its saved settings; fix
media access or reconnect the original AnkiConnect endpoint before resuming a
failed batch. A new batch can use different settings and will skip existing words.

## Dictionaries and operation

The first analysis downloads Sudachi Core (about 72 MB compressed) and English
JMdict (about 11 MB compressed). Later analyses use local copies. Only public
dictionary files are downloaded; subtitles and media are analyzed locally.
If JMdict is unavailable, tokenization still works and meanings can be entered
manually. **Update dictionary** refreshes JMdict; analyze again to use the update.

Sudachi.rs is pinned to upstream 0.6.11 (`90fd6068c80c2fc3b63e0dbab0e341475bad4d8f`),
and Core to `20260723`. No Python installation is required by Nagare. The expanded
Core dictionary is cached under `$DATA_DIR/word-mining/`; the compressed JMdict
archive, analysis workspaces, known words, and batch progress are in
`$DATA_DIR/nagare.sqlite`. Preserve the data directory across deployments.
For an offline installation, populate these caches beforehand or point
`SUDACHI_DICT_PATH` at a compatible Sudachi system dictionary. A missing custom
dictionary produces an error instead of downloading a replacement.

Sudachi and its dictionary are maintained by Works Applications. The downloaded
package's license notices are retained beside the cached dictionary; see
[SudachiDict's license and acknowledgments](https://github.com/WorksApplications/SudachiDict).
JMdict is © the Electronic Dictionary Research and Development Group, distributed
under [CC BY-SA 4.0](https://www.edrdg.org/edrdg/licence.html), obtained through
[jmdict-simplified](https://github.com/scriptin/jmdict-simplified). Attribution
appears in the workspace and generated meaning fields. Edited definitions may
differ from the original dictionary entries.

The workflow is inspired by [Anki Miner](https://github.com/0xzerolight/anki_miner).

## Validation

Run `cargo test` and `npm test` / `npm run build` in `frontend`. The explicit native
tokenizer test downloads Core when necessary:

```sh
cargo test word_mining::analysis::tests::groups_inflections -- --ignored
```

After building the frontend and `cargo build`, run
`python scripts/test-word-mining.py`. It creates synthetic media, a disposable
database under `target`, and a local mock Anki service. It checks real dictionary
analysis and media extraction, custom fields, duplicate skipping, interrupted
responses, pause/resume, process restart recovery, and audio-only mining.
`--serve` keeps the fixture available for browser testing until interrupted.
It never connects to the user's Anki collection.
