import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, nextCall, waitFor } from "./support/harness.js";
import { JoinError } from "../index.js";

test("a denied topic surfaces a join error without disturbing an already-joined topic", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const client = createClient(rpc, { sub: "denied-topic-bob" });
  t.after(() => client.close());

  const granted = await client.join("rooms:public-room");
  await client.track(granted, { status: "online" });
  await waitFor(() => Object.keys(granted.state).length === 1, 5_000, "the initial track to land");

  await assert.rejects(
    () => client.join("rooms:denied-private-room"),
    (error) => {
      assert.ok(error instanceof JoinError, "rejects with a JoinError");
      return true;
    },
  );

  const resynced = nextCall((callback) => granted.onSync(callback));
  const stillAlive = Promise.race([
    resynced.then(() => "resynced"),
    new Promise((resolve) => setTimeout(() => resolve("no resync"), 500)),
  ]);
  assert.equal(await stillAlive, "no resync", "the denied join did not force a resnapshot of the granted topic");
  assert.deepEqual(Object.keys(granted.state), ["denied-topic-bob"], "the granted topic's state survives untouched");
});
