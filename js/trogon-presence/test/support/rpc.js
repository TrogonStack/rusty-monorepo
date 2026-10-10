import { connect } from "node:net";

/** A client for the Rust test harness's newline-delimited JSON RPC sidecar. */
export class RpcClient {
  constructor(address) {
    const [host, port] = address.split(":");
    this._socket = connect({ host, port: Number(port) });
    this._socket.setEncoding("utf8");
    this._nextId = 1;
    this._pending = new Map();
    this._buffer = "";
    this._ready = new Promise((resolve, reject) => {
      this._socket.once("connect", resolve);
      this._socket.once("error", reject);
    });
    this._socket.on("data", (chunk) => this._onData(chunk));
    this._socket.on("error", (error) => this._failAll(error));
    this._socket.on("close", () => this._failAll(new Error("rpc connection closed")));
  }

  _onData(chunk) {
    this._buffer += chunk;
    let index = this._buffer.indexOf("\n");
    while (index >= 0) {
      const line = this._buffer.slice(0, index);
      this._buffer = this._buffer.slice(index + 1);
      index = this._buffer.indexOf("\n");
      if (line.trim().length === 0) continue;
      const message = JSON.parse(line);
      const pending = this._pending.get(message.id);
      if (pending == null) continue;
      this._pending.delete(message.id);
      if (message.ok) pending.resolve(message);
      else pending.reject(new Error(message.error ?? "rpc command failed"));
    }
  }

  _failAll(error) {
    for (const pending of this._pending.values()) pending.reject(error);
    this._pending.clear();
  }

  /**
   * @param {string} cmd
   * @param {Record<string, unknown>} [fields]
   */
  async call(cmd, fields = {}) {
    await this._ready;
    const id = this._nextId;
    this._nextId += 1;
    const request = { id, cmd, ...fields };
    const reply = new Promise((resolve, reject) => this._pending.set(id, { resolve, reject }));
    this._socket.write(`${JSON.stringify(request)}\n`);
    return reply;
  }

  close() {
    this._socket.end();
  }
}

export function rpcAddress() {
  const address = process.env.PRESENCE_RPC_ADDR;
  if (address == null) throw new Error("PRESENCE_RPC_ADDR is not set");
  return address;
}

export function wsUrl() {
  const url = process.env.PRESENCE_WS_URL;
  if (url == null) throw new Error("PRESENCE_WS_URL is not set");
  return url;
}
