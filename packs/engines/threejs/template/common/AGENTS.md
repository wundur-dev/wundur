# {{name}}

A 3D browser game made with [Three.js](https://threejs.org) and [Vite](https://vite.dev).

## Run

```sh
npm install
npm run dev
```

`npm run build` writes the game to `dist/`. `vite.config.js` sets `base: "./"` so the build uses
relative paths: keep it, or a play link (Share in Wundur Studio) can't load the game.

## Layout

- `index.html`: the page the game runs in.
- `src/main.js`: the renderer, scene, camera and loop.
- `public/`: models (glTF/GLB), textures and sounds, served as they are.

## Notes for agents

- Load files with relative paths (`assets/hero.png`, not `/assets/hero.png`): a path from the site's root breaks on a play link.
- Import add-ons from `three/addons/...` (loaders, controls, post-processing).
- Load models as GLB with `GLTFLoader`; check each model you make or change with Wundur's `look_at_model`.
- Keep the game runnable after each change: `npm run build` must pass.
