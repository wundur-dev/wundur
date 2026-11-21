# {{name}}

A 3D browser game made with [Three.js](https://threejs.org) and [Vite](https://vite.dev).

## Run

```sh
npm install
npm run dev
```

`npm run build` writes the game to `dist/`.

## Layout

- `index.html`: the page the game runs in.
- `src/main.js`: the renderer, scene, camera and loop.
- `public/`: models (glTF/GLB), textures and sounds, served as they are.

## Notes for agents

- Import add-ons from `three/addons/...` (loaders, controls, post-processing).
- Load models as GLB with `GLTFLoader`; check each model you make or change with Wundur's `look_at_model`.
- Keep the game runnable after each change: `npm run build` must pass.
