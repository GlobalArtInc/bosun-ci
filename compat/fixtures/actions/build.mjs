import { readdirSync } from "node:fs";
import { join } from "node:path";
import { build } from "esbuild";

const pure = {
  name: "pure",
  setup(context) {
    context.onResolve({ filter: /^@actions\/(exec|http-client|io)(\/|$)/ }, async (args) => {
      if (args.pluginData) {
        return undefined;
      }
      const resolved = await context.resolve(args.path, {
        kind: args.kind,
        importer: args.importer,
        resolveDir: args.resolveDir,
        pluginData: true,
      });
      return { ...resolved, sideEffects: false };
    });
  },
};

const entryPoints = readdirSync("src").flatMap((action) =>
  readdirSync(join("src", action)).map((file) => ({
    in: join("src", action, file),
    out: join(action, "dist", file.replace(/\.js$/, "")),
  })),
);

await build({
  entryPoints,
  outdir: ".",
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node16",
  minifyWhitespace: true,
  legalComments: "none",
  plugins: [pure],
});
