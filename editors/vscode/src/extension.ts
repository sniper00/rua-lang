import * as path from "path";
import {
  workspace,
  window,
  commands,
  ExtensionContext,
  WorkspaceFolder,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(ctx: ExtensionContext): Promise<void> {
  await startClient(ctx);

  ctx.subscriptions.push(
    commands.registerCommand("rua.restartServer", async () => {
      await stopClient();
      await startClient(ctx);
      window.showInformationMessage("Rua language server restarted.");
    }),
  );
}

export function deactivate(): Thenable<void> | undefined {
  return stopClient();
}

async function startClient(_ctx: ExtensionContext): Promise<void> {
  const config = workspace.getConfiguration("rua");
  const command = resolveServerPath(config.get<string>("server.path", "rua-lsp"));
  const args = config.get<string[]>("server.args", []);

  // The server speaks stdio JSON-RPC (see crates/rua-lsp). One process
  // serves every workspace folder; the server indexes each folder on init.
  const serverOptions: ServerOptions = {
    run: { command, args, transport: TransportKind.stdio },
    debug: { command, args, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "rua" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.{rua,ruai}"),
    },
    outputChannel: window.createOutputChannel("Rua Language Server"),
  };

  client = new LanguageClient(
    "rua",
    "Rua Language Server",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
  } catch (err) {
    window.showErrorMessage(
      `Failed to start the Rua language server (\`${command}\`). ` +
        `Build it with \`cargo build -p rua-lsp --bin rua-lsp --features lsp\` and ` +
        `set \`rua.server.path\`, or add it to PATH. Details: ${String(err)}`,
    );
  }
}

async function stopClient(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
}

/**
 * Expand `${workspaceFolder}` and make a workspace-relative path absolute so a
 * locally-built server (e.g. `${workspaceFolder}/target/debug/rua-lsp`) works
 * out of the box. Bare names like `rua-lsp` are left untouched for PATH lookup.
 */
function resolveServerPath(raw: string): string {
  const folder: WorkspaceFolder | undefined = workspace.workspaceFolders?.[0];
  const root = folder?.uri.fsPath ?? "";
  let p = raw.replace(/\$\{workspaceFolder\}/g, root);
  if ((p.includes("/") || p.includes("\\")) && !path.isAbsolute(p) && root) {
    p = path.join(root, p);
  }
  return p;
}
