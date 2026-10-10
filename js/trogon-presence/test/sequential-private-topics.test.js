import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, waitFor } from "./support/harness.js";

test("joining a second private topic after the first still allows commands and diffs on both", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const client = createClient(rpc, { sub: "sequential-private-bob" });
  t.after(() => client.close());

  const first = await client.join("rooms:sequential-private-first");
  await waitFor(() => first.glue.installed != null, 5_000, "the first topic's initial snapshot to install");
  const connectionIdAfterFirst = client.connectionId;

  // Joining a second topic after the first has already settled is the normal "topic-set
  // changed" path: it mints a new connection id and reconnects, which must not leave the
  // shared request/reply inbox pointed at the old, now-unauthorized connection id.
  const second = await client.join("rooms:sequential-private-second");
  await waitFor(() => second.glue.installed != null, 5_000, "the second topic's initial snapshot to install");
  assert.notEqual(
    client.connectionId.toString(),
    connectionIdAfterFirst.toString(),
    "joining a new topic mints a new connection id",
  );

  await client.track(first, { status: "online-first" });
  await client.track(second, { status: "online-second" });
  await waitFor(() => first.state["sequential-private-bob"]?.metas[0]?.status === "online-first", 5_000, "the first topic's track diff");
  await waitFor(() => second.state["sequential-private-bob"]?.metas[0]?.status === "online-second", 5_000, "the second topic's track diff");

  await client.update(first, { status: "updated-first" });
  await client.update(second, { status: "updated-second" });
  await waitFor(() => first.state["sequential-private-bob"]?.metas[0]?.status === "updated-first", 5_000, "the first topic's update diff");
  await waitFor(() => second.state["sequential-private-bob"]?.metas[0]?.status === "updated-second", 5_000, "the second topic's update diff");

  await client.untrack(first);
  await client.untrack(second);
  await waitFor(() => Object.keys(first.state).length === 0, 5_000, "the first topic's untrack diff");
  await waitFor(() => Object.keys(second.state).length === 0, 5_000, "the second topic's untrack diff");
});
