#!/usr/bin/env python3
# A stand-in for the Wundur API's hosted asset flow, for testing the app without the real service.
#   scripts/fake-asset-api.py 18765   then point a test copy at it: settings.json {"account":{"api_base":"http://127.0.0.1:18765"}}
# It also offers the manifests in cards/ as workflows (GET /v1/workflows), except the 3D asset's, and
# answers the image and sound effect cards' calls, so manifest cards can be tried without the service.
# Needs Pillow (pip install pillow). Never logs requests or headers.
import json, time, sys, io, itertools, hashlib, pathlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image, ImageDraw
PORT = int(sys.argv[1])
BASE = f"http://127.0.0.1:{PORT}"
STYLES = ["stylized", "realistic", "low poly", "toon"]
STYLE = {"id": "style", "label": "Style", "type": "enum", "options": STYLES, "default": "stylized", "required": False,
         "help": "Sets the look of the drawings, and the model's paint on Wundur.",
         "option_images": {o: f"/v1/catalog/styles/{o.replace(' ', '-')}.jpg" for o in STYLES}}
BODY = [{"id": "hands", "label": "Hands", "type": "enum", "options": ["auto", "human-like", "as described"], "default": "auto",
         "help": "Auto gives animals with arms hand-like hands where that makes sense, which rigs and animates better. 'As described' keeps paws, claws or wings."},
        {"id": "feet", "label": "Feet", "type": "enum", "options": ["auto", "as described"], "default": "auto",
         "help": "Auto plants the feet with every toe and claw on the ground, which builds and rigs best. 'As described' keeps raised claws, spurs and the like."},
        {"id": "mouth", "label": "Mouth", "type": "enum", "options": ["closed", "open"], "default": "closed",
         "help": "Closed rigs and animates best (a rig can open it). Open shows the teeth, but the model keeps it open."}]
NOTES = {"id": "notes", "label": "Anything else", "type": "text", "help": "Colours, materials, details to keep."}
CATALOG = {"version": "test-2026-11", "kinds": [
  {"id": "humanoid", "label": "Humanoid", "description": "People and anything standing on two legs.",
   "fields": [STYLE, {"id": "height", "label": "Height", "type": "enum", "options": ["child", "adult", "giant"], "default": "adult"}] + BODY + [NOTES],
   "views": 4, "supports_3d": True, "engines": ["trellis2", "tripo_hd", "wundur"], "default_engine": "trellis2"},
  {"id": "creature", "label": "Creature", "description": "Animals and monsters.",
   "fields": [STYLE, {"id": "legs", "label": "Stands on", "type": "enum", "options": ["two legs", "four legs", "other"], "default": "four legs"}] + BODY + [NOTES],
   "views": 4, "supports_3d": True, "engines": ["trellis2", "wundur"], "default_engine": "trellis2"},
  {"id": "prop", "label": "Prop", "description": "Things to hold, wear or place.", "fields": [STYLE, NOTES], "views": 3, "supports_3d": True, "engines": ["trellis2", "wundur"], "default_engine": "trellis2"},
  {"id": "place", "label": "Place", "description": "A room, a building, a landscape.", "fields": [STYLE, NOTES], "views": 1, "supports_3d": False, "engines": [], "offers": ["concept_art"]}],
 "engines": [
  {"id": "trellis2", "label": "Standard", "credits": 1, "plans": ["free", "monthly"], "seconds": 60, "best_for": ["props", "creatures"], "help": "Fast and reliable; simpler textures. Uses the three-quarter view.", "license": {"commercial": True, "label": ""}, "multiview": False, "finishes": []},
  {"id": "tripo_hd", "label": "High detail", "credits": 3, "plans": ["monthly"], "seconds": 180, "best_for": ["detailed characters"], "help": "Finer detail; slower.", "license": {"commercial": True, "label": ""}, "multiview": True, "finishes": []},
  {"id": "wundur", "label": "Wundur (preview)", "credits": 1, "plans": ["free", "monthly"], "seconds": 90, "best_for": ["stylized and low-poly characters"], "help": "Our own engine, in preview.", "license": {"commercial": False, "label": "Preview: not yet cleared for commercial use"}, "multiview": True, "finishes": ["smooth", "faceted"]}],
 "details": [{"id": "light", "label": "Light", "triangles": 8000, "help": "Phones, crowds and background characters; low poly's default."},
             {"id": "standard", "label": "Standard", "triangles": 30000, "help": "Most game characters and props."},
             {"id": "high", "label": "High", "triangles": 100000, "help": "Heroes and close-ups, on PC and console."}],
 "default_detail": "standard", "faceted_detail": "light",
 "riggers": [], "sounds": []}
TRIANGLES = {d["id"]: d["triangles"] for d in CATALOG["details"]}
def style_jpg(name):
    colors = {"stylized": "#e07b39", "realistic": "#5d6d7e", "low-poly": "#3aa56e", "toon": "#a55dd1"}
    im = Image.new("RGB", (224, 152), colors.get(name, "#888")); d = ImageDraw.Draw(im)
    d.polygon([(112, 20), (180, 130), (44, 130)], fill="white"); d.text((10, 8), name, fill="white")
    b = io.BytesIO(); im.save(b, "JPEG"); return b.getvalue()
def suggested(desc):
    words = [w for w in desc.split(",")[0].split() if w.lower() not in ("a", "an", "the")][:3]
    return " ".join(words).capitalize() or "Asset"
assets, models = {}, {}
ids = itertools.count(1)
def png(label, color):
    im = Image.new("RGB", (300, 600), color); d = ImageDraw.Draw(im)
    d.ellipse((110, 60, 190, 140), fill="white"); d.rectangle((120, 150, 180, 400), fill="white")
    d.text((20, 560), label, fill="white"); b = io.BytesIO(); im.save(b, "PNG"); return b.getvalue()
FILES = {}
# The workflows offered: the reference manifests beside this script's folder, each with its version (a
# hash of its content) set as the service sets it.
def workflows():
    found = {}
    for path in sorted((pathlib.Path(__file__).resolve().parent.parent / "cards").glob("*.json")):
        if path.name.endswith(".scenario.json") or path.stem == "asset-3d": continue
        m = json.loads(path.read_text())
        m["version"] = hashlib.sha256(json.dumps({k: v for k, v in m.items() if k != "version"}, sort_keys=True).encode()).hexdigest()[:8]
        found[m["id"]] = m
    return found
# A second of quiet, as a WAV: enough for a sound card to save and play.
def quiet_wav():
    import struct
    n = 8000
    return b"RIFF" + struct.pack("<I", 36 + n) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, 8000, 8000, 1, 8) + b"data" + struct.pack("<I", n) + bytes([128]) * n
def asset_view(a):
    now = time.time()
    for s in a["sets"]:
        if s["status"] == "generating" and now - s["t"] > 3:
            s["status"] = "ready"
            color = ["#3a6ea5", "#a5503a", "#3aa56e"][len(a["sets"]) % 3]
            s["views"] = []
            for lab in ["Front", "Side", "Back", "Three-quarter"]:
                name = f"{s['id']}-{lab}.png"; FILES[name] = png(lab, color)
                s["views"].append({"label": lab, "image_id": "img_" + name, "url": f"{BASE}/files/{name}", "width": 300, "height": 600})
    for m in [models[k] for k in a["models"]]:
        if m["status"] == "queued" and now - m["t"] > 20:
            m["status"] = "running"; m.pop("note", None)
        if m["status"] == "running" and now - m["t"] > (28 if m["engine"] == "wundur" else 8):
            m["status"] = "succeeded"; m["preview_url"] = f"{BASE}/files/preview.glb"
    out = {k: v for k, v in a.items() if k != "models"}
    out["sets"] = [{k: v for k, v in s.items() if k != "t"} for s in a["sets"]]
    out["models"] = [{k: v for k, v in models[m].items() if k != "t"} for m in a["models"]]
    return out
def new_set(a, made_from):
    s = {"id": f"set_{next(ids)}", "status": "generating", "made_from": made_from, "views": [], "approved": False, "attempts_left": 4, "t": time.time()}
    a["sets"].append(s); return s
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, obj=None, raw=None, ctype="application/json"):
        body = raw if raw is not None else json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", ctype); self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def body(self):
        n = int(self.headers.get("content-length") or 0); return json.loads(self.rfile.read(n) or b"{}") if n else {}
    def do_GET(self):
        p = self.path.split("?")[0].strip("/").split("/")
        if p[:1] == ["files"]:
            return self.send(200, raw=FILES.get(p[1], b"glTF-test"), ctype="application/octet-stream")
        if p == ["v1", "catalog"]: return self.send(200, CATALOG)
        if p[:3] == ["v1", "catalog", "styles"]: return self.send(200, raw=style_jpg(p[3].rsplit(".", 1)[0]), ctype="image/jpeg")
        if p == ["v1", "workflows"]:
            return self.send(200, {"workflows": [{"id": m["id"], "version": m["version"], "schema": m.get("schema", 1), "label": m.get("label", ""), "icon": m.get("icon", "")} for m in workflows().values()]})
        if p[:2] == ["v1", "workflows"] and len(p) == 3:
            m = workflows().get(p[2])
            return self.send(200, m) if m else self.send(404, {"error": {"code": "unknown_workflow", "message": "No such workflow."}})
        if p == ["v1", "me"]: return self.send(200, {"plan": "free", "allowance": {"images": {"used": 1, "limit": 5, "left": 4}, "model_credits": {"used": 0, "limit": 3, "left": 3}}})
        if p[:2] == ["v1", "assets"] and len(p) == 3 and p[2] in assets: return self.send(200, asset_view(assets[p[2]]))
        self.send(404, {"error": {"code": "not_found", "message": "No such thing."}})
    def do_POST(self):
        p = self.path.split("?")[0].strip("/").split("/"); b = self.body()
        # The image and sound effect cards' calls. A prompt of "refuse" is refused, to see a card say so.
        if p in (["v1", "images", "generations"], ["v1", "audio", "sound-effects"]):
            if (b.get("prompt") or b.get("text") or "").strip() == "refuse":
                return self.send(402, {"error": {"code": "image_allowance_used", "message": "This month's images are used up."}})
            time.sleep(1.5)
            if p[1] == "images":
                FILES["made.png"] = png(b.get("prompt", "")[:18], (212, 168, 60)); return self.send(200, {"url": f"{BASE}/files/made.png", "images_left": 3})
            FILES["made.wav"] = quiet_wav(); return self.send(200, {"url": f"{BASE}/files/made.wav", "duration_seconds": b.get("duration_seconds", 2), "sound_seconds_left": 117})
        if p == ["v1", "assets"]:
            refs = b.get("references", [])
            if len(refs) > 3: return self.send(422, {"error": {"code": "invalid_request", "message": "At most 3 references."}})
            for n, r in enumerate(refs):
                try: FILES[f"ref-{n}.png"] = __import__("base64").b64decode(r.split(",")[-1])
                except Exception: return self.send(400, {"error": {"code": "bad_image", "message": "Unreadable."}})
            a = {"id": f"as_{next(ids)}", "kind": b["kind"], "name": b.get("name") or suggested(b["description"]), "description": b["description"], "fields": b.get("fields", {}), "sets": [], "models": [],
                 "references": [{"image_id": f"img_ref{n}", "url": f"{BASE}/files/ref-{n}.png"} for n in range(len(refs))]}
            assets[a["id"]] = a; new_set(a, {"kind": "new"}); return self.send(202, asset_view(a))
        if p[:2] == ["v1", "assets"] and len(p) >= 4:
            a = assets[p[2]]
            if p[3] == "sets" and len(p) == 4:
                mf = {"kind": "change", "from": b["from"], "instruction": b["instruction"]} if "instruction" in b else {"kind": "retry", "from": b["from"]}
                new_set(a, mf); return self.send(202, asset_view(a))
            if p[3] == "sets" and p[5] == "approve":
                for s in a["sets"]: s["approved"] = s["id"] == p[4]
                return self.send(200, asset_view(a))
            if p[3] == "models" and len(p) == 4:
                approved = next((s for s in a["sets"] if s["approved"]), None)
                if not approved: return self.send(409, {"error": {"code": "no_approved_set", "message": "Approve a set first."}})
                if approved["attempts_left"] <= 0: return self.send(409, {"error": {"code": "attempts_used", "message": "Attempts used."}})
                approved["attempts_left"] -= 1
                engine = b.get("engine") or "trellis2"
                credits = {"trellis2": 1, "tripo_hd": 3}.get(engine, 1)
                finishes = next(e["finishes"] for e in CATALOG["engines"] if e["id"] == engine)
                if b.get("finish") and b["finish"] not in finishes: return self.send(422, {"error": {"code": "unknown_finish", "message": "That engine has no finishes."}})
                detail = b.get("detail") or CATALOG["default_detail"]
                if detail not in TRIANGLES: return self.send(422, {"error": {"code": "unknown_detail", "message": "No such detail."}})
                m = {"id": f"job_{next(ids)}", "asset_id": a["id"], "set_id": approved["id"], "engine": engine, "status": "running", "imported": False, "credits": credits, "t": time.time(),
                     "detail": detail, "face_limit": TRIANGLES[detail], "finish": (b.get("finish") or (finishes[0] if finishes else None))}
                # The Wundur engine's GPU is asleep: queued a while, with the service's note.
                if engine == "wundur":
                    m["status"] = "queued"; m["note"] = "Starting the GPU: the first model after a quiet spell takes about 10 minutes."
                models[m["id"]] = m; a["models"].append(m["id"]); return self.send(202, {k: v for k, v in m.items() if k != "t"})
            if p[3] == "models" and len(p) == 6:
                m = models[p[4]]
                if p[5] == "import":
                    m["imported"] = True; m["credits_charged"] = m["credits"]
                    return self.send(200, {"glb_url": f"{BASE}/files/model.glb", "model_credits_left": 2})
                if p[5] == "reject": return self.send(200, {"id": m["id"]})
        self.send(404, {"error": {"code": "not_found", "message": "No such thing."}})
ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
