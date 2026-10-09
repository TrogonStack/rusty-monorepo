import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, waitFor } from "./support/harness.js";

test("join receives an empty snapshot, then a live diff for its own track matches phoenix.js semantics", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const client = createClient(rpc, { sub: "join-and-diff-bob" });
  t.after(() => client.close());

  const view = await client.join("rooms:join-and-diff");
  await waitFor(() => view.glue.installed != null, 5_000, "the initial snapshot to install");
  assert.deepEqual(Object.keys(view.state), [], "snapshot starts empty");
  assert.deepEqual(view.list(), [], "phoenix list() starts empty");

  await client.track(view, { status: "online" });
  await waitFor(() => Object.keys(view.state).length === 1, 5_000, "the tracked diff to land");

  assert.deepEqual(Object.keys(view.state), ["join-and-diff-bob"]);
  const metas = view.state["join-and-diff-bob"].metas;
  assert.equal(metas.length, 1);
  assert.equal(metas[0].status, "online");
  assert.equal(typeof metas[0].phx_ref, "string");
  assert.ok(metas[0].phx_ref.length > 0, "server assigns a phx_ref");

  const keyed = view.list((key, presence) => ({ key, count: presence.metas.length }));
  assert.deepEqual(keyed, [{ key: "join-and-diff-bob", count: 1 }]);
});
