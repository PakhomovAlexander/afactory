# The release key

`release.pub` is the minisign public key every release build embeds (`build.rs`); `af self`
verifies a release's `SHA256SUMS.minisig` against it before trusting any checksum, for every
release from 0.8.0 on. The matching secret key never enters this repository: it lives only in
the `MINISIGN_SECRET_KEY` Actions secret, and the release workflow refuses to publish a build
that embeds no key or a release it cannot sign.

Create the pair once, on the maintainer's machine:

```sh
minisign -G -W -p release.pub -s release.key         # -W: no password, the secret store guards it
gh secret set MINISIGN_SECRET_KEY --repo PakhomovAlexander/afactory < release.key
git add crates/reviewctl/keys/release.pub             # commit the public key only
shred -u release.key 2>/dev/null || rm -P release.key  # or keep it offline for rotation
```

Rotation is a new pair, a new `release.pub` committed in a release, and the old secret removed:
binaries built before the rotation keep verifying older releases with the key they embed.

Developer builds without `release.pub` verify checksums only and say so in `af self status`
(`key: none`); `AF_RELEASE_KEY=<file>` points such a build at a key for testing.
