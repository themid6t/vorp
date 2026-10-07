# CI and releases

Vorp uses one long-lived branch, `main`. There is no `dev` branch and no
staging environment.

| Event | What runs |
| --- | --- |
| Pull request with non-Markdown changes | Dashboard type check and build, then format, Clippy, all-feature tests, dependency policy |
| Push or merge to `main` with non-Markdown changes | The same checks, then static Linux amd64 and arm64 release builds, kept as artifacts for 30 days |
| Markdown-only pull request or push | A lightweight passing check; the Rust gates and builds are skipped |
| Annotated `vX.Y.Z` tag on `main` | Publish that commit's builds as a signed release |

The Markdown-only shortcut keeps the `rust` check green, so it can be required
by branch protection without leaving documentation PRs pending.

## Publishing a release

Set `version` in the root `Cargo.toml`, merge, wait for CI to pass on `main`,
then tag that commit:

```sh
git tag -a v0.0.2 -m "v0.0.2" <commit> && git push origin v0.0.2
```

`release.yml` never rebuilds. It checks that the tag is annotated, on `main`,
and matches the crate version, then downloads both binaries from that commit's
successful CI run. Build artifacts expire after 30 days, so tag within that
window. Markdown-only commits produce no artifact, so tag the latest commit
that built; it may be an ancestor of the `main` tip. Never move or delete a
tag.

The workflow packs the binaries as `vorp_<version>_linux_<arch>.tar.gz` and
writes `release-manifest.json` with each archive's sha256 and size. It signs
the manifest with the release GPG key (fingerprint
`8D623B104588BCF08D40CD85A90F7A794E9AC93F`, public half in `vorp.asc`) and
uploads it to `s3://get-vorp` (`ap-south-1`). The archives go first, then
`vorp.asc`, the signature, the manifest, and last `install.sh` and
`version.txt`. A bucket policy makes objects public, and ACLs are disabled.
The `vorp-release-ci` IAM user can only read, write and delete objects in that
bucket. The GitHub Release gets the archives, a `SHA256SUMS` file, the
manifest and its signature.

Repository secrets: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`GPG_PRIVATE_KEY` and `GPG_KEY_ID`.

## Installing and upgrading

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
```

The installer checks the key fingerprint, the manifest signature, the
archive's size and sha256, and that `vorp --version` matches. It keeps the
previous binary as `/usr/local/bin/vorp.rollback`. `VORP_DOWNLOAD_BASE` points
it at a mirror or a local test server.

To upgrade a relay, rerun the installer on the host, then
`sudo systemctl restart vorp`. Nothing deploys automatically.
