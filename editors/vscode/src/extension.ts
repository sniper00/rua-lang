import * as path from "path";
import { spawn } from "child_process";
import {
  workspace,
  window,
  commands,
  Disposable,
  ExtensionContext,
  OutputChannel,
  ProgressLocation,
  Range,
  Uri,
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
  trace: string;
  workspaceSettings: RuaWorkspaceSettings[];
}

interface RuaWorkspaceSettings {
  projectIndex: number;
  workspaceFolder: string;
}

interface RuaLocationArgument {
  uri: string;
  range: {
    start: { line: number; character: number };
    end: { line: number; character: number };
  };
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

  const buildOutput = window.createOutputChannel("Rua Build");
  const projectConfigWatcher = workspace.createFileSystemWatcher("**/.ruarc.toml");
  const reloadProjectConfig = async (): Promise<void> => {
    await sendConfigurationChange();
  };

  ctx.subscriptions.push(
    commands.registerCommand("rua.restartServer", async () => {
      await stopClient();
      await startClient(ctx);
      window.showInformationMessage("Rua language server restarted.");
    }),
    commands.registerCommand("rua.buildFile", (resource?: Uri) =>
      buildFile(resource, buildOutput),
    ),
    commands.registerCommand(
      "rua.openLocation",
      (target: RuaLocationArgument | undefined) => openLocation(target),
    ),
    workspace.onDidChangeConfiguration(async (event) => {
      if (!event.affectsConfiguration("rua")) {
        return;
      }
      await sendConfigurationChange();
    }),
    projectConfigWatcher,
    projectConfigWatcher.onDidCreate(reloadProjectConfig),
    projectConfigWatcher.onDidChange(reloadProjectConfig),
    projectConfigWatcher.onDidDelete(reloadProjectConfig),
    buildOutput,
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

async function openLocation(
  target: RuaLocationArgument | undefined,
): Promise<void> {
  if (!target?.uri || !target.range) {
    return;
  }
  const range = new Range(
    target.range.start.line,
    target.range.start.character,
    target.range.end.line,
    target.range.end.character,
  );
  const document = await workspace.openTextDocument(Uri.parse(target.uri));
  await window.showTextDocument(document, { selection: range });
}

async function sendConfigurationChange(): Promise<void> {
  const running = client;
  if (!running) {
    return;
  }
  await running.sendNotification("workspace/didChangeConfiguration", {
    settings: { rua: readInitializationSettings() },
  });
  testState.configurationNotifications += 1;
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
  return {
    trace: config.get<string>("trace.server", "off"),
    workspaceSettings: folders.map((folder, projectIndex) => ({
      projectIndex,
      workspaceFolder: folder.uri.toString(),
    })),
  };
}

async function buildFile(
  resource: Uri | undefined,
  output: OutputChannel,
): Promise<boolean> {
  const uri = resource ?? window.activeTextEditor?.document.uri;
  if (!uri || uri.scheme !== "file" || path.extname(uri.fsPath) !== ".rua") {
    window.showErrorMessage("Select a .rua file to build.");
    return false;
  }

  const openDocument = workspace.textDocuments.find(
    (document) => document.uri.toString() === uri.toString(),
  );
  if (openDocument?.isDirty && !(await openDocument.save())) {
    window.showErrorMessage(`Could not save ${path.basename(uri.fsPath)}.`);
    return false;
  }

  const folder = workspace.getWorkspaceFolder(uri);
  const config = workspace.getConfiguration("rua", uri);
  const command = resolveToolPath(
    config.get<string>("compiler.path", "ruac"),
    folder,
  );
  const extraArgs = config.get<string[]>("compiler.args", []);
  const args = ["build", uri.fsPath, ...extraArgs];
  const cwd = folder?.uri.fsPath ?? path.dirname(uri.fsPath);

  output.appendLine(`> ${[command, ...args].map(quoteArgument).join(" ")}`);
  const exitCode = await window.withProgress(
    {
      location: ProgressLocation.Notification,
      title: `Building ${path.basename(uri.fsPath)}`,
    },
    () => runCompiler(command, args, cwd, output),
  );
  output.appendLine(`ruac exited with code ${exitCode}\n`);

  if (exitCode !== 0) {
    output.show(true);
    window.showErrorMessage(
      `Rua build failed for ${path.basename(uri.fsPath)}. See the Rua Build output.`,
    );
    return false;
  }

  window.showInformationMessage(`Built ${path.basename(uri.fsPath)}.`);
  return true;
}

function runCompiler(
  command: string,
  args: string[],
  cwd: string,
  output: OutputChannel,
): Promise<number> {
  return new Promise((resolve) => {
    const child = spawn(command, args, { cwd, stdio: "pipe" });
    child.stdout.on("data", (chunk: Buffer) => output.append(chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => output.append(chunk.toString()));
    child.once("error", (error) => {
      output.appendLine(`Failed to start ${command}: ${String(error)}`);
      resolve(-1);
    });
    child.once("close", (code) => resolve(code ?? -1));
  });
}

function quoteArgument(value: string): string {
  return /\s/.test(value) ? JSON.stringify(value) : value;
}

/**
 * Expand `${workspaceFolder}` and make a workspace-relative path absolute so a
 * a locally-built tool (e.g. `${workspaceFolder}/target/debug/ruac`) works out
 * of the box. Bare executable names are left untouched for PATH lookup.
 */
function resolveServerPath(raw: string): string {
  return resolveToolPath(raw, workspace.workspaceFolders?.[0]);
}

function resolveToolPath(
  raw: string,
  folder: WorkspaceFolder | undefined,
): string {
  const root = folder?.uri.fsPath ?? "";
  let p = raw.replace(/\$\{workspaceFolder\}/g, root);
  if (
    (p.includes("/") || p.includes("\\")) &&
    !path.isAbsolute(p) &&
    root
  ) {
    p = path.join(root, p);
  }
  return p;
}
