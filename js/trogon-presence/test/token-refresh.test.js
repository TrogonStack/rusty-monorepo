import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, sleep, waitFor } from "./support/harness.js";

test("a session refreshed before its deadline keeps the connection alive past the original deadline", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const shortSessionSecs = 5;
  const client = createClient(rpc, { sub: "token-refresh-bob", sessionSecs: shortSessionSecs });
  t.after(() => client.close());

  const view = await client.join("rooms:token-refresh");
  await client.track(view, { status: "online" });
  assert.ok(client.connectionId != null, "join mints a connection id");

  await sleep(2_000);
  const reply = await rpc.call("refresh", {
    tenant: client.identity.tenant,
    sub: client.identity.sub,
    sid: client.identity.sid,
    connection_id: client.connectionId.toString(),
    session_secs: 30,
  });
  client.currentToken = reply.token;

  await sleep((shortSessionSecs - 2) * 1_000 + 1_500);

  const kept = await client.update(view, { status: "still online" });
  assert.equal(kept.holder.toString(), client.holderId.toString());
  await waitFor(
    () => view.state["token-refresh-bob"]?.metas[0]?.status === "still online",
    5_000,
    "the post-refresh update to land",
  );
});
