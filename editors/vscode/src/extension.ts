import * as path from "path";
import { spawn } from "child_process";
import {
  workspace,
  window,
  commands,
  Disposable,
  ExtensionContext,
  WorkspaceFolder,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let clientResources: Disposable[] = [];
let serverProcessClosed: Promise<void> | undefined;

interface RuaInitializationSettings {
  library: string[];
  libraryMounts: Record<string, string>;
  sysroot: string | undefined;
  trace: string;
  workspaceSettings: RuaWorkspaceSettings[];
}

interface RuaWorkspaceSettings {
  projectIndex: number;
  workspaceFolder: string;
  library: string[];
  libraryMounts: Record<string, string>;
  sysroot: string | undefined;
}

interface ExtensionTestState {
  starts: number;
  stops: number;
  configurationNotifications: number;
  activeResources: number;
  command: string | undefined;
  args: string[];
  settings: RuaInitializationSettings | undefined;
  workspaceFolders: string[];
}

const testState: ExtensionTestState = {
  starts: 0,
  stops: 0,
  configurationNotifications: 0,
  activeResources: 0,
  command: undefined,
  args: [],
  settings: undefined,
  workspaceFolders: [],
};

export async function activate(ctx: ExtensionContext): Promise<void> {
  await startClient(ctx);

  ctx.subscriptions.push(
    commands.registerCommand("rua.restartServer", async () => {
      await stopClient();
      await startClient(ctx);
      window.showInformationMessage("Rua language server restarted.");
    }),
    workspace.onDidChangeConfiguration(async (event) => {
      if (!event.affectsConfiguration("rua")) {
        return;
      }
      const running = client;
      if (!running) {
        return;
      }
      await running.sendNotification("workspace/didChangeConfiguration", {
        settings: { rua: readInitializationSettings() },
      });
      testState.configurationNotifications += 1;
    }),
  );

  if (process.env.RUA_EXTENSION_TEST === "1") {
    ctx.subscriptions.push(
      commands.registerCommand("rua.__testState", () =>
        JSON.parse(JSON.stringify(testState)),
      ),
      commands.registerCommand("rua.__testDeactivate", () => stopClient()),
    );
  }
}

export function deactivate(): Thenable<void> | undefined {
  return stopClient();
}

async function startClient(_ctx: ExtensionContext): Promise<void> {
  await stopClient();
  const config = workspace.getConfiguration("rua");
  const command = resolveServerPath(config.get<string>("server.path", "rua-lsp"));
  const args = config.get<string[]>("server.args", []);
  const outputChannel = window.createOutputChannel("Rua Language Server");
  const fileWatcher = workspace.createFileSystemWatcher("**/*.{rua,ruai}");
  clientResources = [outputChannel, fileWatcher];

  const settings = readInitializationSettings();

  testState.starts += 1;
  testState.command = command;
  testState.args = [...args];
  testState.settings = settings;
  testState.workspaceFolders =
    workspace.workspaceFolders?.map((folder) => folder.uri.toString()) ?? [];

  // The server speaks stdio JSON-RPC (see crates/rua-lsp). One process
  // serves every workspace folder; the server indexes each folder on init.
  const serverOptions: ServerOptions = async () => {
    const child = spawn(command, args, { stdio: "pipe" });
    serverProcessClosed = new Promise((resolve) => {
      child.once("close", () => resolve());
    });
    return child;
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "rua" }],
    initializationOptions: {
      rua: {
        ...settings,
      },
    },
    synchronize: {
      fileEvents: fileWatcher,
    },
    outputChannel,
  };

  client = new LanguageClient(
    "rua",
    "Rua Language Server",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
    testState.activeResources = clientResources.length;
  } catch (err) {
    client = undefined;
    disposeClientResources();
    window.showErrorMessage(
      `Failed to start the Rua language server (\`${command}\`). ` +
        `Build it with \`cargo build -p rua-lsp --bin rua-lsp --features lsp\` and ` +
        `set \`rua.server.path\`, or add it to PATH. Details: ${String(err)}`,
    );
  }
}

async function stopClient(): Promise<void> {
  const running = client;
  const processClosed = serverProcessClosed;
  client = undefined;
  try {
    if (running) {
      await running.stop();
      // LanguageClient completes the protocol stop before every child stream
      // callback has drained. Keep the output channel alive until the process
      // emits its semantic `close` event.
      await processClosed;
      testState.stops += 1;
    }
  } finally {
    if (serverProcessClosed === processClosed) {
      serverProcessClosed = undefined;
    }
    disposeClientResources();
  }
}

function disposeClientResources(): void {
  for (const resource of clientResources.splice(0)) {
    resource.dispose();
  }
  testState.activeResources = clientResources.length;
}

function readInitializationSettings(): RuaInitializationSettings {
  const config = workspace.getConfiguration("rua");
  const folders = workspace.workspaceFolders ?? [];
  const workspaceSettings = folders.map((folder, projectIndex) =>
    readWorkspaceSettings(folder, projectIndex),
  );
  const library = folders.length === 0
    ? config
        .get<string[]>("library", [])
        .map((configuredPath) => resolveWorkspacePath(configuredPath))
    : [];
  const libraryMounts = folders.length === 0
    ? Object.fromEntries(
        Object.entries(config.get<Record<string, string>>("libraryMounts", {})).map(
          ([name, configuredPath]) => [name, resolveWorkspacePath(configuredPath)],
        ),
      )
    : {};
  const configuredSysroot = config.get<string>("sysroot", "");
  return {
    library,
    libraryMounts,
    sysroot: folders.length === 0 && configuredSysroot
      ? resolveWorkspacePath(configuredSysroot)
      : undefined,
    trace: config.get<string>("trace.server", "off"),
    workspaceSettings,
  };
}

function readWorkspaceSettings(
  folder: WorkspaceFolder,
  projectIndex: number,
): RuaWorkspaceSettings {
  const config = workspace.getConfiguration("rua", folder.uri);
  const library = config
    .get<string[]>("library", [])
    .map((configuredPath) => resolveWorkspacePath(configuredPath, true, folder));
  const libraryMounts = Object.fromEntries(
    Object.entries(config.get<Record<string, string>>("libraryMounts", {})).map(
      ([name, configuredPath]) => [
        name,
        resolveWorkspacePath(configuredPath, true, folder),
      ],
    ),
  );
  const configuredSysroot = config.get<string>("sysroot", "");
  return {
    projectIndex,
    workspaceFolder: folder.uri.toString(),
    library,
    libraryMounts,
    sysroot: configuredSysroot
      ? resolveWorkspacePath(configuredSysroot, true, folder)
      : undefined,
  };
}

/**
 * Expand `${workspaceFolder}` and make a workspace-relative path absolute so a
 * locally-built server (e.g. `${workspaceFolder}/target/debug/rua-lsp`) works
 * out of the box. Bare names like `rua-lsp` are left untouched for PATH lookup.
 */
function resolveServerPath(raw: string): string {
  return resolveWorkspacePath(raw, false);
}

function resolveWorkspacePath(
  raw: string,
  resolveBare = true,
  folder: WorkspaceFolder | undefined = workspace.workspaceFolders?.[0],
): string {
  const root = folder?.uri.fsPath ?? "";
  let p = raw.replace(/\$\{workspaceFolder\}/g, root);
  if (
    (resolveBare || p.includes("/") || p.includes("\\")) &&
    !path.isAbsolute(p) &&
    root
  ) {
    p = path.join(root, p);
  }
  return p;
}
