import { once } from "node:events";
import { readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { TestBurrow } from "./burrow";

/** Real loopback RSS responses; query tokens select distinct feeds whose
 * safe display URLs deliberately collide. No production network is used. */
export async function localFeeds() {
  const requests = { first: 0, second: 0 };
  const server = createServer((request, response) => {
    const first = new URL(request.url!, "http://localhost").searchParams.get("token") === "first-secret";
    requests[first ? "first" : "second"]++;
    const name = first ? "first" : "second";
    const items = Array.from({ length: first ? 1 : 2 }, (_, i) =>
      `<item><guid isPermaLink="false">${name}-${i}</guid><title>${name} item ${i}</title><description>Local feed item</description></item>`).join("");
    response.writeHead(200, { "Content-Type": "application/rss+xml" });
    response.end(`<?xml version="1.0"?><rss version="2.0"><channel><title>Local ${name}</title><link>https://example.test/</link><description>Browser fixture</description>${items}</channel></rss>`);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No feed fixture port");
  const base = `http://127.0.0.1:${address.port}/rss`;
  return {
    base, requests,
    first: `${base}?token=first-secret#first-fragment`,
    second: `${base}?token=second-secret#second-fragment`,
    credential: base.replace("http://", "http://fixture-user:credential-secret@") + "?token=credential-query",
    async dispose() {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    },
  };
}

/** Edit the actual TOML while stopped, preserving identity, accounts and
 * boards. This exercises the shipped restart-only mapping contract. */
export async function restartWithFeeds(server: TestBurrow, enabled: boolean, feeds: Record<string, string>) {
  await server.stop();
  const path = join(server.dataDir, "burrow.toml");
  let config = await readFile(path, "utf8");
  config = config.replace(/^syndication_enabled\s*=.*\n/gm, "").replace(/\n\[syndication_feeds\][\s\S]*$/, "");
  config = `syndication_enabled = ${enabled}\n${config}\n[syndication_feeds]\n`;
  config += Object.entries(feeds).map(([url, board]) => `${JSON.stringify(url)} = ${JSON.stringify(board)}`).join("\n") + "\n";
  await writeFile(path, config);
  await server.start();
}

export async function feedBurrow(name: string, feeds: Record<string, string>) {
  const server = await TestBurrow.create(name, "235b96");
  try {
    await server.ctl("account-create", "feed-admin", "theme-e2e-password", "admin");
    for (const board of new Set(Object.values(feeds))) await server.ctl("board-create", board, board);
    await restartWithFeeds(server, false, feeds);
    return server;
  } catch (error) {
    await server.dispose();
    throw error;
  }
}
