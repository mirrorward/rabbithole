import { spawn, execFile, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { writeFileSync } from "node:fs";
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

const exec = promisify(execFile);
const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

// Bind first rather than choosing a fixed test port. The brief release/bind
// interval is unavoidable without a socket-activation interface on burrow.
async function unusedPort(): Promise<number> {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No test port");
  const port = address.port;
  await new Promise<void>((resolve, reject) => server.close((e) => e ? reject(e) : resolve()));
  return port;
}

/** A real local burrow, with isolated data and public discovery disabled. */
export class TestBurrow {
  private child?: ChildProcess;
  private exited?: Promise<unknown>;
  private log = "";
  private readonly env: NodeJS.ProcessEnv;

  private constructor(
    readonly name: string,
    readonly dataDir: string,
    readonly httpURL: string,
    readonly wsURL: string,
    private readonly binary: string,
  ) {
    this.env = { ...process.env, RUST_LOG: "warn" };
    // Local development overrides must not redirect a test to another data
    // directory, bind it publicly, or enable public tracker announcements.
    for (const key of Object.keys(this.env)) {
      if (key.startsWith("RABBITHOLE_")) delete this.env[key];
    }
  }

  static async create(
    name: string,
    accent: string,
    options: { rateLimits?: "default" | "generous"; spaDist?: string } = {},
  ): Promise<TestBurrow> {
    const binary = resolve(process.env.BURROW_BIN!);
    const dist = resolve(options.spaDist ?? process.env.SPA_DIST!);
    await access(binary);
    await access(join(dist, "index.html"));
    // CI provides an owned root so its final cleanup can find any fixture
    // left behind by a terminated Playwright worker.
    const fixtureRoot = resolve(process.env.BURROW_E2E_ROOT ?? tmpdir());
    await mkdir(fixtureRoot, { recursive: true });
    const dataDir = await mkdtemp(join(fixtureRoot, "rh-browser-e2e-"));
    const http = await unusedPort();
    let ws = await unusedPort();
    while (ws === http) ws = await unusedPort();
    const burrow = new TestBurrow(name, dataDir, `http://127.0.0.1:${http}`, `ws://127.0.0.1:${ws}`, binary);
    await writeFile(join(dataDir, "burrow.toml"), [
      `name = ${JSON.stringify(name)}`,
      'quic_addr = "127.0.0.1:0"',
      `ws_addr = "127.0.0.1:${ws}"`,
      "http_enabled = true",
      `http_addr = "127.0.0.1:${http}"`,
      `http_web_root = ${JSON.stringify(dist)}`,
      "announce_enabled = false",
      // Theme/route tests isolate their behavior from anti-abuse budgets.
      // The keepalive acceptance test leaves every production limit intact.
      ...(options.rateLimits === "default" ? [] : [
        "ratelimit_conn_burst = 256",
        "ratelimit_conn_per_min = 600",
      ]),
      `theme_accent = ${JSON.stringify(accent)}`,
      '[theme_tokens_shared]',
      '"--rh-radius" = "0.75rem"',
      "",
    ].join("\n"));
    try {
      await burrow.start();
      await burrow.ctl("account-create", "theme-viewer", "theme-e2e-password", "user");
      return burrow;
    } catch (error) {
      await burrow.dispose();
      throw error;
    }
  }

  async start(): Promise<void> {
    if (this.child) throw new Error("Test burrow is already running");
    const child = spawn(this.binary, ["--data-dir", this.dataDir, "run"], {
      env: this.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    this.child = child;
    let spawnError: Error | undefined;
    child.on("error", (error) => { spawnError = error; });
    this.exited = new Promise((resolve) => {
      child.once("exit", resolve);
      child.once("error", resolve);
    });
    for (const stream of [child.stdout, child.stderr]) {
      stream?.on("data", (chunk) => {
        this.log = (this.log + chunk).slice(-32_000);
        // Preserve a bounded log if the worker is interrupted before attaching it.
        try { writeFileSync(join(this.dataDir, "server.log"), this.log); } catch { /* teardown */ }
      });
    }
    if (child.pid) await writeFile(join(this.dataDir, "server.pid"), String(child.pid));
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      if (spawnError || child.exitCode !== null) break;
      try {
        await access(join(this.dataDir, "ctl.sock"));
        const response = await fetch(this.httpURL, { signal: AbortSignal.timeout(500) });
        if (response.ok && (await response.text()).includes("RabbitHole")) return;
      } catch { /* The listening socket may not exist yet. */ }
      await delay(40);
    }
    throw new Error(`Test burrow ${this.name} did not start: ${spawnError ?? ""}\n${this.log}`);
  }

  async ctl(command: string, ...args: string[]): Promise<unknown> {
    const { stdout } = await exec(this.binary, ["--data-dir", this.dataDir, "ctl", command, ...args], {
      env: this.env,
      timeout: 10_000,
    });
    return JSON.parse(stdout);
  }

  async stop(): Promise<void> {
    const child = this.child;
    if (!child) return;
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGINT");
    let force: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        this.exited,
        new Promise((resolve) => {
          force = setTimeout(() => { child.kill("SIGKILL"); resolve(undefined); }, 5_000);
        }),
      ]);
      await this.exited;
    } finally {
      if (force) clearTimeout(force);
      this.child = undefined;
      await rm(join(this.dataDir, "server.pid"), { force: true });
    }
  }

  async restartWithAccent(accent: string): Promise<void> {
    await this.stop();
    const path = join(this.dataDir, "burrow.toml");
    const config = await readFile(path, "utf8");
    if (!/^theme_accent\s*=.*$/m.test(config)) throw new Error("No saved theme_accent fixture field");
    await writeFile(path, config.replace(/^theme_accent\s*=.*$/m, `theme_accent = ${JSON.stringify(accent)}`));
    await this.start();
  }

  /** Seed the existing account preference; this is setup, not a UI workflow. */
  async disableAccountTheme(): Promise<void> {
    const { DatabaseSync } = await import("node:sqlite");
    const db = new DatabaseSync(join(this.dataDir, "burrow.db"));
    try {
      db.exec("PRAGMA busy_timeout = 5000");
      const changed = db.prepare("UPDATE accounts SET theme_server_disabled = 1 WHERE login = ?")
        .run("theme-viewer");
      if (changed.changes !== 1) throw new Error("Theme opt-out fixture account was not found");
    } finally {
      db.close();
    }
  }

  async dispose(): Promise<void> {
    await this.stop();
    await rm(this.dataDir, { recursive: true, force: true });
  }

  logs(): string { return this.log; }
}
