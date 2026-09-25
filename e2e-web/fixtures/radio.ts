import { createServer, type Server } from "node:http";
import { createConnection, type Socket } from "node:net";
import { once } from "node:events";
import { TestBurrow } from "./burrow";

// Real source-ingest sessions advertise stations through the real burrow.
// Their advertised public delivery base points at a local WAV fixture. This
// exercises the browser's native decoder/play promises without requiring an
// external encoder, a public stream, or changing the radio engine.
export class RadioFixture {
  private readonly sources: Socket[] = [];
  private readonly delivery: Server;
  readonly broken = new Set<string>();
  readonly requests: string[] = [];
  private constructor(readonly server: TestBurrow) {
    const rate = 8000;
    const pcm = Buffer.alloc(rate * 2 * 120);
    for (let n = 0; n < pcm.length / 2; n++) pcm.writeInt16LE(Math.round(Math.sin(n * 2 * Math.PI * 220 / rate) * 1000), n * 2);
    const header = Buffer.alloc(44);
    header.write("RIFF"); header.writeUInt32LE(pcm.length + 36, 4); header.write("WAVEfmt ", 8);
    header.writeUInt32LE(16, 16); header.writeUInt16LE(1, 20); header.writeUInt16LE(1, 22);
    header.writeUInt32LE(rate, 24); header.writeUInt32LE(rate * 2, 28);
    header.writeUInt16LE(2, 32); header.writeUInt16LE(16, 34); header.write("data", 36); header.writeUInt32LE(pcm.length, 40);
    const wav = Buffer.concat([header, pcm]);
    this.delivery = createServer((req, res) => {
      const path = new URL(req.url!, "http://localhost").pathname;
      this.requests.push(path);
      if (this.broken.has(path)) { res.writeHead(503); res.end(); return; }
      const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range ?? "");
      const start = range ? Number(range[1]) : 0;
      const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
      if (start >= wav.length || end < start) { res.writeHead(416); res.end(); return; }
      res.writeHead(range ? 206 : 200, {
        "Content-Type": "audio/wav", "Content-Length": end - start + 1,
        "Accept-Ranges": "bytes", "Cache-Control": "no-store",
        ...(range ? { "Content-Range": `bytes ${start}-${end}/${wav.length}` } : {}),
      });
      res.end(wav.subarray(start, end + 1));
    });
  }
  static async create(server: TestBurrow): Promise<RadioFixture> {
    const fixture = new RadioFixture(server);
    try {
      fixture.delivery.listen(0, "127.0.0.1");
      await once(fixture.delivery, "listening");
      const address = fixture.delivery.address();
      if (!address || typeof address === "string") throw new Error("No audio fixture port");
      await server.ctl("config-set", "radio_public_base", `http://127.0.0.1:${address.port}`);
      await fixture.station("first", "First station");
      await fixture.station("second", "Second station");
      return fixture;
    } catch (error) { await fixture.dispose(); throw error; }
  }
  async station(slug: string, name: string) {
    const url = new URL(this.server.radioSourceURL!);
    const socket = createConnection({ host: url.hostname, port: Number(url.port) });
    this.sources.push(socket);
    socket.on("error", () => {});
    await once(socket, "connect");
    const auth = Buffer.from("source:radio-e2e-password").toString("base64");
    const ack = new Promise<void>((resolve, reject) => {
      let head = "";
      const timer = setTimeout(() => reject(new Error(`Source handshake timed out: ${head}`)), 10_000);
      const data = (chunk: Buffer) => {
        head += chunk.toString();
        if (!head.includes("\r\n\r\n")) return;
        clearTimeout(timer); socket.off("data", data);
        if (head.includes("200 OK")) resolve(); else reject(new Error(`Source refused: ${head}`));
      };
      socket.on("data", data);
    });
    socket.write(`PUT /${slug} HTTP/1.1\r\nAuthorization: Basic ${auth}\r\nContent-Type: audio/wav\r\nice-name: ${name}\r\n\r\n`);
    await ack;
    // Publish the source's initial title through the real encoder metadata
    // surface, including a pure-DJ mount with no library program.
    await this.metadata(slug, name);
  }
  async metadata(slug: string, title: string) {
    const auth = Buffer.from("source:radio-e2e-password").toString("base64");
    const response = await fetch(`${this.server.radioSourceURL}/admin/metadata?mount=/${slug}&mode=updinfo&song=${encodeURIComponent(title)}`, {
      headers: { Authorization: `Basic ${auth}` }, signal: AbortSignal.timeout(5000),
    });
    const body = await response.text();
    if (!response.ok || !body.includes("<return>1</return>")) throw new Error(`Metadata update failed: ${response.status} ${body}`);
  }
  async dispose() {
    for (const socket of this.sources) socket.destroy();
    this.delivery.closeAllConnections();
    await new Promise<void>((resolve) => this.delivery.close(() => resolve()));
  }
}
