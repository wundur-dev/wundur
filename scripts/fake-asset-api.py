#!/usr/bin/env python3
# A stand-in for the Wundur API's hosted asset flow, for testing the app without the real service.
#   scripts/fake-asset-api.py 18765   then point a test copy at it: settings.json {"account":{"api_base":"http://127.0.0.1:18765"}}
# Needs Pillow (pip install pillow). Never logs requests or headers.
import json, time, sys, io, itertools
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from PIL import Image, ImageDraw
PORT = int(sys.argv[1])
BASE = f"http://127.0.0.1:{PORT}"
CATALOG = {"version": "test", "kinds": [
  {"id": "humanoid", "label": "Humanoid", "description": "People and anything on two legs.",
   "fields": [{"id": "style", "label": "Style", "type": "enum", "options": ["realistic", "stylized", "pixel"], "default": "stylized"},
              {"id": "notes", "label": "Anything else", "type": "text"}],
   "views": 3, "supports_3d": True, "engines": ["trellis2", "tripo_hd"], "default_engine": "trellis2"},
  {"id": "place", "label": "Place", "description": "A room, a building, a landscape.", "fields": [], "views": 1, "supports_3d": False, "engines": [], "offers": ["concept_art"]}],
 "engines": [
  {"id": "trellis2", "label": "Standard", "credits": 1, "plans": ["free", "monthly"], "seconds": 60, "best_for": ["props", "creatures"], "help": "Fast and reliable; simpler textures.", "license": {"commercial": True, "label": ""}},
  {"id": "tripo_hd", "label": "High detail", "credits": 3, "plans": ["monthly"], "seconds": 180, "best_for": ["detailed characters"], "help": "Finer detail; slower.", "license": {"commercial": True, "label": ""}}],
 "riggers": [], "sounds": []}
assets, models = {}, {}
ids = itertools.count(1)
def png(label, color):
    im = Image.new("RGB", (300, 600), color); d = ImageDraw.Draw(im)
    d.ellipse((110, 60, 190, 140), fill="white"); d.rectangle((120, 150, 180, 400), fill="white")
    d.text((20, 560), label, fill="white"); b = io.BytesIO(); im.save(b, "PNG"); return b.getvalue()
FILES = {}
def asset_view(a):
    now = time.time()
    for s in a["sets"]:
        if s["status"] == "generating" and now - s["t"] > 3:
            s["status"] = "ready"
            color = ["#3a6ea5", "#a5503a", "#3aa56e"][len(a["sets"]) % 3]
            s["views"] = []
            for lab in ["Front", "Side", "Back"]:
                name = f"{s['id']}-{lab}.png"; FILES[name] = png(lab, color)
                s["views"].append({"label": lab, "image_id": "img_" + name, "url": f"{BASE}/files/{name}", "width": 300, "height": 600})
    for m in [models[k] for k in a["models"]]:
        if m["status"] == "running" and now - m["t"] > 4:
            m["status"] = "succeeded"; m["preview"] = {"mesh": {"url": f"{BASE}/files/preview.glb", "reduced": True}}
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
        if p == ["v1", "me"]: return self.send(200, {"plan": "free", "allowance": {"images": {"used": 1, "limit": 5, "left": 4}, "model_credits": {"used": 0, "limit": 3, "left": 3}}})
        if p[:2] == ["v1", "assets"] and len(p) == 3 and p[2] in assets: return self.send(200, asset_view(assets[p[2]]))
        self.send(404, {"error": {"code": "not_found", "message": "No such thing."}})
    def do_POST(self):
        p = self.path.strip("/").split("/"); b = self.body()
        if p == ["v1", "assets"]:
            a = {"id": f"as_{next(ids)}", "kind": b["kind"], "name": b["name"], "description": b["description"], "fields": b.get("fields", {}), "sets": [], "models": []}
            assets[a["id"]] = a; new_set(a, {"kind": "new"}); return self.send(202, asset_view(a))
        if p[:2] == ["v1", "assets"] and len(p) >= 4:
            a = assets[p[2]]
            if p[3] == "sets" and len(p) == 4:
                mf = {"kind": "change", "from": b["from"], "instruction": b["instruction"]} if "instruction" in b else {"kind": "retry", "from": b["from"]}
                new_set(a, mf); return self.send(202, asset_view(a))
            if p[3] == "sets" and p[5] == "approve":
                for s in a["sets"]: s["approved"] = s["id"] == p[4]
                return self.send(200, asset_view(a))
            if p[3] == "models":
                m = {"id": f"md_{next(ids)}", "asset": a["id"], "set": b["set"], "engine": b["engine"], "status": "running", "imported": False, "attempts_left": 3, "t": time.time()}
                models[m["id"]] = m; a["models"].append(m["id"]); return self.send(202, {k: v for k, v in m.items() if k != "t"})
        if p[:2] == ["v1", "models"] and len(p) == 4:
            m = models[p[2]]
            if p[3] == "reroll":
                n = dict(m, id=f"md_{next(ids)}", status="running", preview=None, attempts_left=m["attempts_left"] - 1, t=time.time(), engine=b.get("engine", m["engine"]))
                models[n["id"]] = n; assets[m["asset"]]["models"].append(n["id"]); return self.send(202, {k: v for k, v in n.items() if k != "t"})
            if p[3] == "import":
                m["imported"] = True; return self.send(200, {"id": m["id"], "imported": True, "files": [{"kind": "model", "format": "glb", "url": f"{BASE}/files/model.glb"}], "model_credits_left": 2})
            if p[3] == "reject": return self.send(200, {"id": m["id"]})
        self.send(404, {"error": {"code": "not_found", "message": "No such thing."}})
ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
