// look_at_model: a picture of 3D models for the agent to look at, drawn by Wundur on the CPU
// (no window, GPU or browser): a row per model, a column per fixed camera angle, the same
// camera and light for all, with each model's facts (triangles, size, textures, animations)
// and warnings (textures that failed to load, extensions, materials that render dark).
export default async function (args, kx) {
  // Paths, one per line or comma-separated; "path @+x" says which way that model faces.
  const entries = String(args.models || args.model || "")
    .split(/[\n,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  if (!entries.length) return kx.fail("look_at_model needs models: one GLB or glTF path in the project, or several separated by commas.");
  const labels = String(args.labels || "").split(",").map((s) => s.trim());
  const models = entries.map((entry, i) => {
    const m = entry.match(/^(.*?)\s*@\s*([+-]?[xzXZ])$/);
    const path = (m ? m[1] : entry).trim();
    return { path, label: labels[i] || undefined, facing: m ? m[2].toLowerCase() : undefined };
  });
  for (const m of models) {
    if (!/\.(glb|gltf)$/i.test(m.path)) return kx.fail(`${m.path} isn't a GLB or glTF model.`);
  }
  const views = String(args.views || "").split(",").map((s) => s.trim()).filter(Boolean);
  const stem = models.length === 1 ? models[0].path.split("/").pop().replace(/\.(glb|gltf)$/i, "") : "compare";
  const name = kx.slug(args.name || `${stem}${args.focus ? "-" + args.focus : ""}`);
  const size = parseInt(args.size, 10);
  const zoom = parseFloat(args.zoom);
  let r;
  try {
    r = kx.renderModel({
      models,
      views,
      facing: args.facing || undefined,
      animation: args.animation || undefined,
      time: args.time || undefined,
      focus: args.focus || undefined,
      zoom: Number.isFinite(zoom) ? zoom : undefined,
      sameScale: !!args.same_scale,
      cell: Number.isFinite(size) ? size : undefined,
      out: `.wundur/looks/${name}.png`,
    });
  } catch (e) {
    return kx.fail(`Couldn't draw it: ${e.message}`);
  }
  const shown = views.length ? views.join(", ") : "front, left side, back, three-quarter";
  const head = `${r.image} (${r.width}×${r.height}): ${models.length === 1 ? "one model" : `${models.length} models, a row each`}; columns ${shown}${args.focus ? `, close up on ${args.focus}` : ""}.`;
  const notes = [];
  if (r.failed) notes.push(`${r.failed} couldn't be loaded.`);
  if (r.warnings) notes.push(`${r.warnings} warning${r.warnings === 1 ? "" : "s"}: see below.`);
  return { text: `${head}${notes.length ? " " + notes.join(" ") : ""}\n\n${r.report}`, images: [r.image] };
}
