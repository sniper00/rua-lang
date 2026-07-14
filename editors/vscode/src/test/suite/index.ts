import * as assert from "assert";
import * as path from "path";
import * as vscode from "vscode";

interface ExtensionTestState {
  starts: number;
  stops: number;
  configurationNotifications: number;
  activeResources: number;
  command: string;
  args: string[];
  settings: {
    library: string[];
    libraryMounts: Record<string, string>;
    trace: string;
    workspaceSettings: Array<{
      projectIndex: number;
      workspaceFolder: string;
      library: string[];
      libraryMounts: Record<string, string>;
    }>;
  };
  workspaceFolders: string[];
}

async function state(): Promise<ExtensionTestState> {
  return vscode.commands.executeCommand<ExtensionTestState>("rua.__testState");
}

async function waitUntil(
  predicate: () => Promise<boolean>,
  message: string,
): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (await predicate()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(message);
}

export async function run(): Promise<void> {
  const extension = vscode.extensions.getExtension("moon-rs.rua-lang");
  assert.ok(extension, "Rua extension is installed in the Extension Host");
  await extension.activate();

  const initial = await state();
  assert.strictEqual(initial.starts, 1);
  assert.strictEqual(initial.stops, 0);
  assert.strictEqual(initial.activeResources, 2);
  assert.strictEqual(initial.workspaceFolders.length, 2);
  assert.deepStrictEqual(initial.args, []);
  assert.ok(path.isAbsolute(initial.command));

  const firstRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const secondRoot = vscode.workspace.workspaceFolders?.[1]?.uri.fsPath;
  assert.ok(firstRoot);
  assert.ok(secondRoot);
  assert.deepStrictEqual(initial.settings.library, []);
  assert.deepStrictEqual(initial.settings.libraryMounts, {});
  assert.deepStrictEqual(initial.settings.workspaceSettings[0].library, [
    path.join(firstRoot, "types"),
  ]);
  assert.strictEqual(
    initial.settings.workspaceSettings[0].libraryMounts.alpha_host,
    path.join(firstRoot, "types", "host.ruai"),
  );
  assert.deepStrictEqual(initial.settings.workspaceSettings[1].library, [
    path.join(secondRoot, "beta-types"),
  ]);
  assert.strictEqual(
    initial.settings.workspaceSettings[1].libraryMounts.beta_host,
    path.join(secondRoot, "beta-types", "host.ruai"),
  );
  assert.strictEqual(initial.settings.trace, "off");

  const firstFolder = vscode.workspace.workspaceFolders?.[0];
  assert.ok(firstFolder);
  const config = vscode.workspace.getConfiguration("rua", firstFolder.uri);
  await config.update(
    "library",
    ["${workspaceFolder}/changed"],
    vscode.ConfigurationTarget.WorkspaceFolder,
  );
  await waitUntil(
    async () => (await state()).configurationNotifications >= 1,
    "dynamic Rua configuration was not forwarded to the language server",
  );

  await vscode.commands.executeCommand("rua.restartServer");
  const restarted = await state();
  assert.strictEqual(restarted.starts, 2);
  assert.strictEqual(restarted.stops, 1);
  assert.strictEqual(restarted.activeResources, 2);
  assert.deepStrictEqual(restarted.settings.workspaceSettings[0].library, [
    path.join(firstRoot, "changed"),
  ]);
  assert.deepStrictEqual(restarted.settings.workspaceSettings[1].library, [
    path.join(secondRoot, "beta-types"),
  ]);
  assert.strictEqual(restarted.workspaceFolders.length, 2);

  await vscode.commands.executeCommand("rua.__testDeactivate");
  const stopped = await state();
  assert.strictEqual(stopped.stops, 2);
  assert.strictEqual(stopped.activeResources, 0);
}
