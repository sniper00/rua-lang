import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { runTests } from "@vscode/test-electron";

async function main(): Promise<void> {
  const extensionDevelopmentPath = path.resolve(__dirname, "../..");
  const repositoryRoot = path.resolve(extensionDevelopmentPath, "../..");
  const executable = path.join(
    repositoryRoot,
    "target",
    "debug",
    process.platform === "win32" ? "rua-lsp.exe" : "rua-lsp",
  );
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "rua-vscode-test-"));
  const alpha = path.join(temp, "alpha");
  const beta = path.join(temp, "beta");
  const alphaDeclarations = path.join(alpha, "types");
  const betaDeclarations = path.join(beta, "beta-types");
  fs.mkdirSync(alphaDeclarations, { recursive: true });
  fs.mkdirSync(betaDeclarations, { recursive: true });
  fs.mkdirSync(path.join(alpha, ".vscode"), { recursive: true });
  fs.mkdirSync(path.join(beta, ".vscode"), { recursive: true });
  fs.writeFileSync(path.join(alpha, "main.rua"), "let value = 1;\n");
  fs.writeFileSync(path.join(beta, "main.rua"), "let value = 2;\n");
  fs.writeFileSync(path.join(alphaDeclarations, "host.ruai"), "");
  fs.writeFileSync(path.join(betaDeclarations, "host.ruai"), "");
  fs.writeFileSync(
    path.join(alpha, ".vscode", "settings.json"),
    JSON.stringify({
      "rua.library": ["${workspaceFolder}/types"],
      "rua.libraryMounts": {
        alpha_host: "${workspaceFolder}/types/host.ruai",
      },
    }),
  );
  fs.writeFileSync(
    path.join(beta, ".vscode", "settings.json"),
    JSON.stringify({
      "rua.library": ["${workspaceFolder}/beta-types"],
      "rua.libraryMounts": {
        beta_host: "${workspaceFolder}/beta-types/host.ruai",
      },
    }),
  );

  const workspaceFile = path.join(temp, "multi-root.code-workspace");
  fs.writeFileSync(
    workspaceFile,
    JSON.stringify(
      {
        folders: [{ path: alpha }, { path: beta }],
        settings: {
          "rua.server.path": executable,
          "rua.server.args": [],
          "rua.trace.server": "off",
        },
      },
      null,
      2,
    ),
  );

  process.env.RUA_EXTENSION_TEST = "1";
  delete process.env.ELECTRON_RUN_AS_NODE;
  for (const name of Object.keys(process.env)) {
    if (name.startsWith("VSCODE_")) {
      delete process.env[name];
    }
  }
  try {
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath: path.resolve(__dirname, "suite", "index"),
      launchArgs: [workspaceFile, "--disable-extensions"],
    });
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
