// Linux CI backstop for fixtures whose Playwright worker was interrupted.
// Normal test cleanup stops each server and removes its temporary directory.
import { copyFile, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { join, resolve } from "node:path";

const root = process.env.BURROW_E2E_ROOT;
const binary = process.env.BURROW_BIN;
if (!root || !binary) process.exit(0);
if (process.platform !== "linux") throw new Error("CI fixture cleanup requires Linux /proc process verification");
const entries = await readdir(root, { withFileTypes: true }).catch((error) => {
  if (error.code === "ENOENT") return [];
  throw error;
});
await mkdir("artifacts", { recursive: true });
for (const entry of entries) {
  if (!entry.isDirectory() || !entry.name.startsWith("rh-browser-e2e-")) continue;
  const data = join(resolve(root), entry.name);
  const pid = Number(await readFile(join(data, "server.pid"), "utf8").catch(() => ""));
  if (Number.isSafeInteger(pid) && pid > 0) {
    const argv = (await readFile(`/proc/${pid}/cmdline`, "utf8").catch(() => "")).split("\0");
    const dataFlag = argv.indexOf("--data-dir");
    // Never signal a reused PID or a process outside this exact fixture.
    if (argv[0] === resolve(binary) && dataFlag >= 0 && argv[dataFlag + 1] === data) {
      try { process.kill(pid, "SIGTERM"); } catch (error) { if (error.code !== "ESRCH") throw error; }
      const deadline = Date.now() + 5_000;
      while (Date.now() < deadline) {
        const running = await readFile(`/proc/${pid}/cmdline`, "utf8").catch(() => "");
        if (!running) break;
        await new Promise((done) => setTimeout(done, 50));
      }
      const current = (await readFile(`/proc/${pid}/cmdline`, "utf8").catch(() => "")).split("\0");
      if (current[0] === resolve(binary) && current[current.indexOf("--data-dir") + 1] === data) {
        try { process.kill(pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
      }
    }
  }
  await copyFile(join(data, "server.log"), join("artifacts", `${entry.name}.log`)).catch((error) => {
    if (error.code !== "ENOENT") throw error;
  });
  await rm(data, { recursive: true, force: true });
}
