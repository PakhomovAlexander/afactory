# The release key

`release.pub` is the minisign public key every release build embeds (`build.rs`); `af self`
verifies a release's `SHA256SUMS.minisig` against it before trusting any checksum, and refuses
a release without one. That holds on the lock path too: an install a project's `.af/af.lock`
asks for needs the archive to match both the lock's digest and the signed `SHA256SUMS`
(`verified_by = "lock+signature"`), and happens on demand only as `[self] install_pins` allows
(`"ask"` by default: a confirmation at the terminal, and none without one). `install.sh` checks
the same signature with `minisign` and refuses without it unless `AF_INSTALL_ALLOW_UNSIGNED=1`. The matching secret key never enters this repository: it lives only in
the `MINISIGN_SECRET_KEY` Actions secret, and the release workflow refuses to publish a build
that embeds no key or a release it cannot sign.

Create the pair once, on the maintainer's machine:

```sh
minisign -G -W -p release.pub -s release.key         # -W: no password, the secret store guards it
gh secret set MINISIGN_SECRET_KEY --repo PakhomovAlexander/afactory < release.key
git add crates/af/keys/release.pub                    # commit the public key only
shred -u release.key 2>/dev/null || rm -P release.key  # or keep it offline for rotation
```

Rotation is a new pair, a new `release.pub` committed in a release, and the old secret removed:
binaries built before the rotation keep verifying older releases with the key they embed.

Developer builds without `release.pub` verify checksums only and say so in `af self status`
(`key: none`); `AF_RELEASE_KEY=<file>` points such a build at a key for testing.

## The nightly key

`nightly.pub` is the public half of a separate minisign key the nightly build signs a nightly's
`SHA256SUMS` with; it is added here by hand, and `build.rs` embeds it as `AF_NIGHTLY_KEY` when it
exists. `af self` verifies a nightly against that key alone and a release never against it
([ADR-0267](../../../docs/adr/0267-offer-a-nightly-channel-in-af-self.md)). A build without the
file refuses `af self channel nightly` and every nightly install with one sentence (`this af
carries no nightly key`); `AF_NIGHTLY_KEY=<file>` points a build at a key instead. `install.sh`
embeds the same key as `NIGHTLY_PUB`, empty until the file exists, and refuses `--nightly` while
it is (`AF_INSTALL_NIGHTLY_KEY=<file>` overrides it).
