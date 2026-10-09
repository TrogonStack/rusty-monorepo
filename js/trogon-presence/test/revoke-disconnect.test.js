import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient } from "./support/harness.js";

function timeout(ms, value) {
  return new Promise((resolve) => setTimeout(() => resolve(value), ms));
}

test("revoking a session disconnects the client's live connection", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const client = createClient(rpc, { sub: "revoke-bob" });
  t.after(() => client.close());

  let sawDisconnect;
  const disconnected = new Promise((resolve) => {
    sawDisconnect = resolve;
  });
  client._watchStatus = function watchStatus() {
    this.statusLoop = (async () => {
      for await (const status of this.connection.status()) {
        if (status.type === "disconnect") sawDisconnect(status);
        if (status.type === "reconnect") {
          for (const view of this.views.values()) view.glue.resync("transport reconnected");
        }
      }
    })();
  };

  const view = await client.join("rooms:revoke-disconnect");
  await client.track(view, { status: "online" });

  await rpc.call("revoke", {
    tenant: client.identity.tenant,
    sub: client.identity.sub,
    sid: client.identity.sid,
  });

  const status = await Promise.race([disconnected, timeout(15_000, null)]);
  assert.ok(status != null, "the revoked session's connection disconnects within the sweep window");
  assert.equal(status.type, "disconnect");
});
