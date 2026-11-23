# {{name}}

A 2D browser game made with [Phaser](https://phaser.io) and [Vite](https://vite.dev).

## Run

```sh
npm install
npm run dev
```

`npm run build` writes the game to `dist/`. `vite.config.js` sets `base: "./"` so the build uses
relative paths: keep it, or a play link (Share in Wundur Studio) can't load the game.

## Layout

- `index.html`: the page the game runs in.
- `src/main.js`: the game's config and its scenes.
- `public/`: images, sprite sheets and sounds, served as they are (`public/assets/hero.png` loads as `assets/hero.png`).

## Notes for agents

- Load files with relative paths (`assets/hero.png`, not `/assets/hero.png`): a path from the site's root breaks on a play link.
- Phaser 4. Check the API against https://docs.phaser.io; Phaser 3 examples mostly still work, but not all.
- Load sprite sheets as atlases (`this.load.atlas(key, png, json)`), in the TexturePacker JSON format, and make animations from their frame names (`this.anims.generateFrameNames`).
- Keep the game runnable after each change: `npm run build` must pass.
