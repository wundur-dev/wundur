import { defineConfig } from "vite";

// Relative paths in the build, so it plays from any folder: a play link serves the game from a
// folder of its own, not the site's root.
export default defineConfig({
  base: "./",
});
