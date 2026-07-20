import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { runTests } from "@vscode/test-electron";

async function main(): Promise<void> {
  const extensionDevelopmentPath = path.resolve(__dirname, "../..");
  const repositoryRoot = path.resolve(extensionDevelopmentPath, "../..");
  const lspExecutable = path.join(
    repositoryRoot,
    "target",
    "debug",
    process.platform === "win32" ? "rua-lsp.exe" : "rua-lsp",
  );
  const compilerExecutable = path.join(
    repositoryRoot,
    "target",
    "debug",
    process.platform === "win32" ? "ruac.exe" : "ruac",
  );
  const bundledTarget = bundledPlatformTarget();
  const bundledDirectory = bundledTarget
    ? path.join(extensionDevelopmentPath, "bin", bundledTarget)
    : undefined;
  const bundledLsp = bundledDirectory
    ? path.join(
        bundledDirectory,
        process.platform === "win32" ? "rua-lsp.exe" : "rua-lsp",
      )
    : undefined;
  const bundledCompiler = bundledDirectory
    ? path.join(
        bundledDirectory,
        process.platform === "win32" ? "ruac.exe" : "ruac",
      )
    : undefined;
  const useBundledToolchain =
    bundledLsp !== undefined &&
    bundledCompiler !== undefined &&
    fs.existsSync(bundledLsp) &&
    fs.existsSync(bundledCompiler);
  if (useBundledToolchain && process.platform !== "win32") {
    fs.chmodSync(bundledLsp, 0o755);
    fs.chmodSync(bundledCompiler, 0o755);
  }
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "rua-vscode-test-"));
  const alpha = path.join(temp, "alpha");
  const beta = path.join(temp, "beta");
  const alphaDeclarations = path.join(alpha, "types");
  const betaDeclarations = path.join(beta, "beta-types");
  fs.mkdirSync(alphaDeclarations, { recursive: true });
  fs.mkdirSync(betaDeclarations, { recursive: true });
  fs.writeFileSync(path.join(alpha, "main.rua"), "let value = 1;\n");
  fs.mkdirSync(path.join(alpha, "domain"), { recursive: true });
  fs.writeFileSync(
    path.join(alpha, "domain", "order.rua"),
    "pub struct OrderRequest { pub sku: String }\n" +
      "impl OrderRequest {\n" +
      "    /// Construct an order request.\n" +
      "    pub fn new(sku: String) -> OrderRequest { OrderRequest { sku: sku } }\n" +
      "}\n",
  );
  fs.writeFileSync(
    path.join(alpha, "requests.rua"),
    "use domain::order::OrderRequest;\n" +
      'let requests = [OrderRequest::new("book-001")];\n',
  );
  fs.writeFileSync(path.join(beta, "main.rua"), "let value = 2;\n");
  fs.writeFileSync(path.join(alphaDeclarations, "host.ruai"), "");
  fs.writeFileSync(path.join(betaDeclarations, "host.ruai"), "");
  fs.writeFileSync(
    path.join(alpha, ".ruarc.toml"),
    '[workspace]\nlibrary = ["types"]\n\n' +
      '[workspace.library_mounts]\nalpha_host = "types/host.ruai"\n',
  );
  fs.writeFileSync(
    path.join(beta, ".ruarc.toml"),
    '[workspace]\nlibrary = ["beta-types"]\n\n' +
      '[workspace.library_mounts]\nbeta_host = "beta-types/host.ruai"\n',
  );

  const workspaceFile = path.join(temp, "multi-root.code-workspace");
  fs.writeFileSync(
    workspaceFile,
    JSON.stringify(
      {
        folders: [{ path: alpha }, { path: beta }],
        settings: {
          "rua.server.path": useBundledToolchain ? "" : lspExecutable,
          "rua.server.args": [],
          "rua.compiler.path": useBundledToolchain ? "" : compilerExecutable,
          "rua.compiler.args": [],
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

function bundledPlatformTarget(): string | undefined {
  if (process.platform === "linux" && process.arch === "x64") {
    return "linux-x64";
  }
  if (process.platform === "linux" && process.arch === "arm64") {
    return "linux-arm64";
  }
  if (process.platform === "darwin" && process.arch === "x64") {
    return "darwin-x64";
  }
  if (process.platform === "darwin" && process.arch === "arm64") {
    return "darwin-arm64";
  }
  if (process.platform === "win32" && process.arch === "x64") {
    return "win32-x64";
  }
  return undefined;
}
