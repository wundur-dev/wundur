# Game assets

Tools for making game assets with your own keys, for every agent in Wundur. Each service's tools
appear once its key is set in Settings > Tools > Packs; everything is saved in
`assets/generated/` (a setting).

**Images: OpenAI's gpt-image, directly or through Azure OpenAI**

- `make_image`: an image from a description, saved as a PNG and shown to the agent.
- `edit_image`: change an image in the project (recolour, add, remove, restyle), optionally with a mask and reference images.

**Sound: ElevenLabs**

- `sound_effect`: a sound effect from a description, up to 30 seconds, optionally looping.
- `voice_line`: a line spoken by a voice; `list_voices` shows the voices the account has.
- `music`: a track from a description. ElevenLabs' terms limit music in games to plans that include it: check yours before shipping.

**3D models: Tripo**

- `model_from_text`, `model_from_image`: a textured GLB, with a preview image the agent can look at. A job of a minute or a few.
- `view_model`: a page to look at a model in (one HTML file next to it, the model inside), opened in Wundur's web panel: turn and zoom, play its animations with the camera following.
- `look_at_model`: a picture of models for the agent itself to look at, drawn by Wundur with no window, GPU or browser: a row per model, a column per camera angle (front, side, back, three-quarter by default), the same camera and light for all, with each model's facts (triangles, size, textures, animations) and warnings (a texture that failed to load shows magenta). Close-ups on a joint or region, and a pose from an animation. Free; saved in `.wundur/looks/`.
- `rig_model`: a skeleton for a model (from one of the above, by its Tripo task, or a GLB or FBX in the project), and optionally Tripo's preset animations (idle, walk, run…). GLB or FBX, with Mixamo, Unity or Unreal bone layouts.

Tools that cost money ask before they run; "Always for this session" stops asking.
