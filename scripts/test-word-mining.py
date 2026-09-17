"""End-to-end mining checks against a disposable Nagare database and mock Anki.

Build frontend and `cargo build` first. Requires ffmpeg/ffprobe and network access
for the first dictionary download. Never connects to a user's Anki collection.
Use --serve to keep the isolated fixture open for browser testing after checks.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
FIELDS = dict(word="Word", reading="Reading", meaning="Meaning", sentence="Sentence",
              audio="SentenceAudio", picture="Picture", source="Source")
CUSTOM = dict(word="Expression", reading="Pronunciation", meaning="Definition",
              sentence="Context", audio="Clip", picture="Image", source="Origin")
LINES = ["猫が魚を食べました。", "猫は魚を食べる。", "犬と森へ行く。",
         "山の上に青い空が見える。", "国際連合で日本語を話します。"]


class MockAnki(BaseHTTPRequestHandler):
    models = {"Vocabulary Mock": list(CUSTOM.values())}
    notes = {100: dict(modelName="Vocabulary Mock", fields={"Expression": "猫"}, tags=[])}
    media = {}
    additions = []
    lose_response = ""
    delay = 0
    added = threading.Event()

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            action, params = request["action"], request.get("params", {})
            result = self.invoke(action, params)
            response = dict(result=result, error=None)
        except Exception as error:
            response = dict(result=None, error=str(error))
        data = json.dumps(response).encode()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass  # Expected when the restart test kills an in-flight client.

    @classmethod
    def invoke(cls, action, params):
        if action == "version":
            return 6
        if action == "deckNames":
            return ["Mining Test"]
        if action == "modelNames":
            return list(cls.models)
        if action == "modelFieldNames":
            return cls.models[params["modelName"]]
        if action == "createModel":
            cls.models[params["modelName"]] = params["inOrderFields"]
            return {"id": 1000}
        if action == "findNotes":
            query = params["query"]
            if query.startswith("tag:"):
                return [i for i, n in cls.notes.items() if query[4:] in n["tags"]]
            if query.startswith('note:"'):
                return [i for i, n in cls.notes.items() if n["modelName"] == query[6:-1]]
            return []
        if action == "notesInfo":
            return [dict(noteId=i, modelName=cls.notes[i]["modelName"], tags=cls.notes[i]["tags"],
                         fields={k: dict(value=v, order=j) for j, (k, v) in enumerate(cls.notes[i]["fields"].items())})
                    for i in params["notes"] if i in cls.notes]
        if action == "storeMediaFile":
            cls.media[params["filename"]] = base64.b64decode(params["data"])
            return params["filename"]
        if action == "retrieveMediaFile":
            data = cls.media.get(params["filename"])
            return base64.b64encode(data).decode() if data else False
        if action == "addNote":
            note = params["note"]
            assert note["deckName"] == "Mining Test"
            assert note["options"]["allowDuplicate"] is False
            assert "nagare::word_miner" in note["tags"]
            key = cls.models[note["modelName"]][0]
            term = note["fields"][key]
            assert not any(n["modelName"] == note["modelName"] and n["fields"].get(key) == term
                           for n in cls.notes.values()), "Duplicate addNote"
            note_id = max(cls.notes) + 1
            cls.notes[note_id] = note
            cls.additions.append(term)
            cls.added.set()
            time.sleep(cls.delay)
            if term == cls.lose_response:
                cls.lose_response = ""
                raise RuntimeError("Simulated lost addNote response after committing")
            return note_id
        if action == "updateNoteFields":
            note = params["note"]
            cls.notes[note["id"]]["fields"].update(note["fields"])
            return None
        if action in ("findCards", "cardsInfo"):
            return []
        if action in ("getNumCardsReviewedToday",):
            return 0
        raise RuntimeError(f"Unsupported mock action: {action}")


def request(url, body=None, method=None):
    data = json.dumps(body, ensure_ascii=False).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=240) as response:
        return json.load(response)


def main():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument("--serve", action="store_true")
    args = args.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="word-mining-smoke-", dir=ROOT / "target"))
    media = directory / "scene.mp4"
    audio = directory / "book.flac"
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0x294b48:s=640x360:r=12:d=20",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=20", "-f", "lavfi", "-i", "sine=frequency=880:duration=20",
                    "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac",
                    "-metadata:s:a:0", "language=eng", "-metadata:s:a:1", "language=jpn", str(media)],
                   check=True, creationflags=flags)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(media), "-map", "0:a:1", str(audio)],
                   check=True, creationflags=flags)
    mock = ThreadingHTTPServer(("127.0.0.1", 0), MockAnki)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    config = dict(listen_address=f"127.0.0.1:{port}", anki={"url": f"http://127.0.0.1:{mock.server_port}"},
                  mining={"static_screenshot_format": "png", "audio_codec": "mp3"})
    (directory / "config.json").write_text(json.dumps(config), encoding="utf-8")
    history = {}
    for ident, title, file in [("video", "言葉の森 · A walk through the forest", media), ("audio", "日本語の物語 · Audiobook", audio)]:
        key = f"jellyfin|{ident}"
        history[key] = dict(history_id=key, server_kind="jellyfin", item_id=ident, title=title,
                            series_name="Mining test", media_source_id=ident, file_path=str(file),
                            duration_ms=20000, subtitle_count=len(LINES), audio_languages=["jpn"],
                            last_position_ms=15000, last_seen="2026-09-17T12:00:00Z")
    track = dict(lines=[dict(index=i * 5, start_ms=i * 3500 + 500, end_ms=i * 3500 + 3000, text=text)
                        for i, text in enumerate(LINES)], offset_ms=0)
    (directory / "history.json").write_text(json.dumps(history, ensure_ascii=False), encoding="utf-8")
    (directory / "subtitle_history.json").write_text(json.dumps({k: track for k in history}, ensure_ascii=False), encoding="utf-8")
    env = {**os.environ, "DATA_DIR": str(directory)}
    cached = ROOT / "data/word-mining/sudachi-core-20260723.dic"
    if cached.exists():
        env["SUDACHI_DICT_PATH"] = str(cached)
    log = (directory / "server.log").open("w", encoding="utf-8")
    process = None

    def start():
        nonlocal process
        process = subprocess.Popen([str(ROOT / ("target/debug/nagare.exe" if os.name == "nt" else "target/debug/nagare"))],
                                   cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT, creationflags=flags)
        for _ in range(100):
            try:
                request(base + "/api/state")
                return
            except (OSError, urllib.error.URLError):
                assert process.poll() is None, f"Nagare exited; inspect {directory / 'server.log'}"
                time.sleep(.1)
        raise AssertionError("Nagare startup timed out")

    def api(path, body=None, method=None):
        result = request(base + path, body, method)
        assert result.get("ok"), result
        return result

    path = "/api/history/jellyfin%7Cvideo/word-mining"

    def wait_job(job_id):
        for _ in range(240):
            job = api(f"/api/word-mining/jobs/{job_id}")["job"]
            if job["status"] != "running":
                return job
            time.sleep(.1)
        raise AssertionError("Mining job timed out")

    def batch(workspace, terms, fields=CUSTOM, model="Vocabulary Mock", **overrides):
        drafts = []
        for term in terms:
            candidate = next(c for c in workspace["candidates"] if c["term"] == term)
            index = candidate["occurrences"][0]
            line = workspace["track"]["lines"][index]
            drafts.append(dict(term=term, reading=candidate["reading"], meaning=candidate["definitions"][0]["meaning"] if candidate["definitions"] else "",
                               sentence=line["text"], first=index, last=index, start_ms=line["start_ms"], end_ms=line["end_ms"]))
        settings = dict(deck="Mining Test", model=model, fields=fields, tags=["integration"],
                        audio_ordinal=None, screenshot=True, animated=False)
        settings.update(overrides)
        return dict(request_id=str(uuid.uuid4()), revision=workspace["revision"], settings=settings, cards=drafts)

    try:
        start()
        assert api(path)["workspace"] is None
        assert "Word" in api("/api/word-mining/anki")["fields"]
        media_info = api(path + "/media")
        assert len(media_info["tracks"]) == 2 and media_info["selected"] == 1
        print("PASS: History loading, note-type discovery, Japanese audio selection", flush=True)
        analyzed = api(path + "/analyze", {"split_mode": "B"})
        workspace = analyzed["workspace"]
        assert not analyzed.get("warning"), analyzed.get("warning")
        eat = next(c for c in workspace["candidates"] if c["term"] == "食べる")
        assert eat["occurrences"] == [0, 1] and eat["reading"] == "たべる"
        assert "eat" in eat["definitions"][0]["meaning"]
        assert any(c["term"] == "行く" for c in workspace["candidates"]), "Do not discard potentially auxiliary content verbs"
        assert workspace["track"]["lines"][1]["index"] == 1
        assert len(api(path + "/term", {"term": "猫が魚", "revision": workspace["revision"]})["workspace"]["candidates"]) == len(workspace["candidates"]) + 1
        api("/api/word-mining/known", {"terms": ["魚"], "status": "ignored"}, "PUT")
        synced = api("/api/word-mining/sync-known", {"model": "Vocabulary Mock", "field": "Expression"})
        assert synced["known"]["猫"] == "in_anki" and synced["known"]["魚"] == "ignored"
        for kind, mime in [("audio", "audio/mpeg"), ("image", "image/png")]:
            preview = api(path + "/preview", dict(kind=kind, start_ms=500, end_ms=3000, audio_ordinal=None))
            assert preview["mime"] == mime and len(base64.b64decode(preview["data"])) > 100
        assert not request(base + path + "/preview", dict(kind="audio", start_ms=-1, end_ms=3000))["ok"]
        print("PASS: Native Sudachi, real JMdict, custom phrases, known-word sync, audio/frame previews", flush=True)

        MockAnki.lose_response = "食べる"
        body = batch(workspace, ["食べる", "魚", "猫"])
        invalid = {**body, "revision": "outdated"}
        assert not request(base + path + "/jobs", invalid)["ok"]
        invalid_audio = {**body, "settings": {**body["settings"], "audio_ordinal": 63}}
        assert not request(base + path + "/jobs", invalid_audio)["ok"]
        assert not request(base + f"/api/word-mining/jobs/{body['request_id']}")["ok"], "Invalid media must not leave a stranded batch"
        job = api(path + "/jobs", body)["job"]
        job = wait_job(job["id"])
        assert job["status"] == "needs_attention", job
        assert [c["status"] for c in job["cards"]] == ["failed", "created", "skipped"], job
        assert api(path + "/jobs", body)["job"]["id"] == job["id"]
        api(f"/api/word-mining/jobs/{job['id']}/resume", {})
        job = wait_job(job["id"])
        assert job["status"] == "complete", job
        assert MockAnki.additions.count("食べる") == 1 and MockAnki.additions.count("魚") == 1
        detail = api(f"/api/review/{job['id']}")
        review = detail["review"]
        assert len(review["cards"]) == 2, detail
        assert review["track"]["lines"][0]["text"] == LINES[0]
        created_note = MockAnki.notes[job["cards"][0]["note_id"]]
        assert created_note["fields"]["Context"] == LINES[0]
        assert "[sound:" in created_note["fields"]["Clip"] and "<img" in created_note["fields"]["Image"]
        assert job["settings"]["audio_ordinal"] == 1
        print("PASS: Custom fields, duplicate skipping, lost-response recovery, request idempotency, review snapshot", flush=True)

        MockAnki.delay = 1
        MockAnki.added.clear()
        job = api(path + "/jobs", batch(workspace, ["犬", "森"]))["job"]
        assert MockAnki.added.wait(15)
        api(f"/api/word-mining/jobs/{job['id']}/pause", {})
        job = wait_job(job["id"])
        assert job["status"] == "paused" and job["cards"][1]["status"] == "pending", job
        MockAnki.delay = 0
        api(f"/api/word-mining/jobs/{job['id']}/resume", {})
        assert wait_job(job["id"])["status"] == "complete"
        print("PASS: Pause after the current card, resume without recreating successful cards", flush=True)

        MockAnki.delay = 2
        MockAnki.added.clear()
        job = api(path + "/jobs", batch(workspace, ["山", "青い"]))["job"]
        assert MockAnki.added.wait(15)
        process.kill()
        process.wait()
        MockAnki.delay = 0
        start()
        job = api(f"/api/word-mining/jobs/{job['id']}")["job"]
        assert job["status"] == "paused", job
        api(f"/api/word-mining/jobs/{job['id']}/resume", {})
        assert wait_job(job["id"])["status"] == "complete"
        assert MockAnki.additions.count("山") == 1 and MockAnki.additions.count("青い") == 1
        assert api(path)["workspace"]["revision"] == workspace["revision"]
        print("PASS: Process restart recovery with an already-committed Anki note", flush=True)

        audio_path = "/api/history/jellyfin%7Caudio/word-mining"
        audio_workspace = api(audio_path + "/analyze", {"split_mode": "A"})["workspace"]
        job = api(audio_path + "/jobs", batch(audio_workspace, ["空"], fields=FIELDS, model="Nagare Vocabulary"))["job"]
        job = wait_job(job["id"])
        assert job["status"] == "complete", job
        note = MockAnki.notes[job["cards"][0]["note_id"]]
        assert "[sound:" in note["fields"]["SentenceAudio"] and not note["fields"].get("Picture")
        print("PASS: Built-in vocabulary note type creation and audio-only mining", flush=True)
        uploaded = api(audio_path + "/analyze", dict(split_mode="C", subtitle_name="replacement.srt",
                       subtitle_text="\ufeff5\n00:00:01,250 --> 00:00:03,750\n国際連合で働く。\n\n19\n00:00:05,000 --> 00:00:07,000\n猫は寝ています。\n"))["workspace"]
        assert uploaded["split_mode"] == "C" and uploaded["subtitle_name"] == "replacement.srt"
        assert uploaded["track"]["lines"][0]["start_ms"] == 1250
        assert len(uploaded["track"]["lines"]) == 2 and uploaded["revision"] != audio_workspace["revision"]
        assert any(c["term"] == "国際連合" for c in uploaded["candidates"]), "Mode C should keep the compound"
        original = request(base + "/api/history/jellyfin%7Caudio/subtitles")
        assert len(original["lines"]) == len(LINES), "Uploads must not replace live/history subtitles"
        assert len(api(f"/api/review/{job['id']}")["review"]["track"]["lines"]) == len(LINES)
        assert not request(base + audio_path + "/jobs", batch(audio_workspace, ["猫"], fields=FIELDS, model="Nagare Vocabulary"))["ok"]
        print("PASS: Subtitle upload, Sudachi compound mode, stale revision rejection, immutable review history", flush=True)
        print(f"All mining integration checks passed. Fixture: {directory}", flush=True)
        if args.serve:
            info = dict(base=base, history_url=base + "/history/jellyfin%7Cvideo/mine", directory=str(directory))
            (ROOT / "target/word-mining-fixture.json").write_text(json.dumps(info), encoding="utf-8")
            print(f"Browser fixture: {info['history_url']}", flush=True)
            while True:
                time.sleep(1)
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        mock.shutdown()
        log.close()


if __name__ == "__main__":
    main()
