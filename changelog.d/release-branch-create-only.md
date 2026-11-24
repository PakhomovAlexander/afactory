- RC cuts and stable promotions now create their release branch only if the remote ref is absent,
  protecting a branch created concurrently. Recovery of an existing branch requires a durable push
  receipt for the exact candidate. An identical commit alone still produces a branch collision.
