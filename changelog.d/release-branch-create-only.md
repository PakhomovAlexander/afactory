- RC cuts and stable promotions now create their release branch only if the remote ref is absent,
  protecting a branch created concurrently and preserving recovery of the exact release candidate.
