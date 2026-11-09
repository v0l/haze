import { defineConfig, loadEnv, type Plugin } from "vite";
import preact from "@preact/preset-vite";
import { viteSingleFile } from "vite-plugin-singlefile";
import { sourceHash } from "./source-hash";

function stampSources(): Plugin {
  return {
    name: "haze-source-hash",
    apply: "build",
    transformIndexHtml: (html) => html.replace("</head>", `<meta name="haze-source" content="${sourceHash(__dirname)}">\n</head>`),
  };
}

export default defineConfig(({ mode }) => {
  const target = loadEnv(mode, process.cwd(), "").HAZE_URL || "http://127.0.0.1:8480";
  return {
    plugins: [preact(), viteSingleFile({ removeViteModuleLoader: true }), stampSources()],
    build: {
      target: "es2022",
      assetsInlineLimit: 100_000_000,
      cssCodeSplit: false,
      reportCompressedSize: false,
      chunkSizeWarningLimit: 100_000,
    },
    server: {
      proxy: Object.fromEntries(["/api", "/metrics"].map((path) => [path, { target, changeOrigin: true }])),
    },
  };
});
