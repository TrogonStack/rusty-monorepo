import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, sleep, waitFor } from "./support/harness.js";

test("the client's own token cache refreshes before the deadline and survives a forced reconnect, with no manual refresh", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  // Meaningfully longer than TokenCache's REFRESH_LEEWAY (5s): a session at or below the
  // leeway leaves no real "refresh ahead of expiry" point to aim for and is a degenerate
  // configuration, not something this test should exercise.
  const shortSessionSecs = 15;
  const client = createClient(rpc, { sub: "token-refresh-bob", sessionSecs: shortSessionSecs });
  t.after(() => client.close());

  const view = await client.join("rooms:token-refresh");
  await client.track(view, { status: "online" });
  const connectionIdAtJoin = client.connectionId;
  assert.ok(connectionIdAtJoin != null, "join mints a connection id");
  const tokenAtJoin = client.currentToken;

  // A second, independent consumer of the connection's status stream: it does not interfere
  // with the client's own internal watcher, it only observes whether a real redial happened.
  let reconnects = 0;
  (async () => {
    for await (const status of client.connection.status()) {
      if (status.type === "reconnect") reconnects += 1;
    }
  })().catch(() => {});

  // Wait past the original JWT's deadline. Neither the test nor the app calls the refresh
  // RPC or assigns `currentToken`: the client's own `TokenCache` must have refreshed
  // proactively, well before this deadline, and again on every disconnect, entirely on its
  // own, for the connection to still be usable here.
  await sleep(shortSessionSecs * 1_000 + 2_000);

  const kept = await waitFor(
    async () => {
      try {
        return await client.update(view, { status: "still online" });
      } catch {
        return null;
      }
    },
    20_000,
    "an update to succeed once the client's own token cache has refreshed past the original deadline",
  );
  assert.equal(kept.holder.toString(), client.holderId.toString());
  assert.notEqual(client.currentToken, tokenAtJoin, "the token cache refreshed its own token without test intervention");
  assert.equal(
    client.connectionId.toString(),
    connectionIdAtJoin.toString(),
    "a plain refresh reuses the same connection id, it does not mint a new one",
  );
  assert.ok(reconnects > 0, "the transport actually redialed past the original JWT deadline");

  await waitFor(
    () => view.state["token-refresh-bob"]?.metas[0]?.status === "still online",
    5_000,
    "the post-refresh update to land",
  );
});
