# CI, staging, and production releases

Vorp uses one long-lived branch, `main`. Keep release tags on commits that have
already passed staging. There is no permanent `dev` branch.

| Event | Checks | Deployment |
| --- | --- | --- |
| Pull request with non-Markdown changes | Dashboard type check and build, then format, Clippy, all-feature tests, dependency policy | None |
| Push/merge to `main` with non-Markdown changes | The same checks, then static Linux amd64 and arm64 release builds | Automatically deploy the amd64 build to staging and check `/healthz` |
| Markdown-only pull request or push | Lightweight successful check | None |
| Annotated `vX.Y.Z` tag on a validated `main` commit | Verify the exact commit passed CI and staging; use its existing builds | Publish them to the `get-vorp` bucket and a GitHub Release. Production deployment is still manual |

The tag is the release decision. The release workflow never rebuilds: it
publishes the binaries CI built for the same commit, which staging already ran.
It rejects tags that are not annotated, not on `main`, or do not match the
crate version. Staging artifacts expire after 30 days, so tag within that
window. Markdown-only commits produce no artifact, so tag the latest commit
that actually passed staging; it may be an ancestor of the `main` tip. Never
move or delete a tag.

## Publishing a release

Set `version` in the root `Cargo.toml`, merge, wait for CI and staging to pass
on `main`, then tag that commit:

```sh
git tag -a v0.0.1 -m "v0.0.1" <commit> && git push origin v0.0.1
```

`release.yml` checks that the tag is annotated, on `main`, and matches the
crate version. It downloads both binaries from that commit's successful CI run,
packs them as `vorp_<version>_linux_<arch>.tar.gz`, and writes
`release-manifest.json` with each archive's sha256 and size. It signs the
manifest with the release GPG key (fingerprint
`8D623B104588BCF08D40CD85A90F7A794E9AC93F`, public half in `vorp.asc`) and
uploads it to `s3://get-vorp` (`ap-south-1`). The archives go first, then the
signature, then the manifest, then `install.sh` and `version.txt`. A bucket
policy makes objects public, and ACLs are disabled. The `vorp-release-ci` IAM
user can only read, write and delete objects in that bucket.

Install or upgrade on any Linux amd64 or arm64 host:

```sh
curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
```

The installer checks the key fingerprint, the manifest signature, the
archive's size and sha256, and that `vorp --version` matches. It keeps the
previous binary as `/usr/local/bin/vorp.rollback`. `VORP_DOWNLOAD_BASE`
points it at a mirror or a local test server.

Repository secrets: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`GPG_PRIVATE_KEY` and `GPG_KEY_ID`.

## Production

The maintainer's production relay is upgraded by hand: rerun the installer on
the host, then `sudo systemctl restart vorp`. The installer keeps the previous
binary as `/usr/local/bin/vorp.rollback`. A nightly systemd timer takes a
`sqlite3 .backup` of the database, checks its integrity, and keeps 14 days.
Automatic production deployment on a tag is not planned before v0.1. If it is
added, it must use credentials separate from staging.

## Current staging deployment

The `CI` workflow builds and type-checks the embedded dashboard, then runs all four Rust gates for pull requests and pushes to
`main` when a non-Markdown file changes. A Markdown-only change keeps a green
`rust` check but skips compilation and deployment. This allows the check to be
required by future branch protection without leaving documentation PRs
pending. A successful code push to `main` builds static Linux amd64 and arm64 binaries
on GitHub-hosted runners and keeps both as artifacts. Only amd64 is deployed. It copies the binary to a dedicated `vorp-deploy` account
over SSH and
invokes `/usr/local/sbin/vorp-deploy-staging` through a narrowly scoped sudo
rule. The server verifies the checksum, replaces `/opt/vorp/vorp`, restarts
`vorp.service`, and checks the HTTPS health endpoint. On failure it restores
the previous binary and restarts the service. The SQLite database and TLS
files are outside this release path.

The systemd unit is versioned as [`deploy/vorp-staging.service`](../deploy/vorp-staging.service)
but is not deployed by CI. After changing it, install it on the host:

```sh
sudo install -m 0644 deploy/vorp-staging.service /etc/systemd/system/vorp.service
sudo systemctl daemon-reload && sudo systemctl restart vorp
```

Repository configuration:

- `STAGING_SSH_KEY` (secret): private key for the dedicated deploy account.
- `STAGING_SSH_HOST` (variable): the staging hostname.
- `STAGING_SSH_KNOWN_HOSTS` (variable): pinned ED25519 host key, checked against
  the staging host before adding it.

The matching public key is restricted from forwarding and interactive TTY use.
Only the deploy script is permitted through sudo. If the host's public address
changes, update DNS; the workflow connects by hostname. GitHub-hosted runner
source IPs change, so staging SSH must be reachable from those runners.
