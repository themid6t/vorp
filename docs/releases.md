# CI, staging, and production releases

Vorp uses one long-lived branch, `main`. Keep release tags on commits that have
already passed staging. There is no permanent `dev` branch.

| Event | Checks | Deployment |
| --- | --- | --- |
| Pull request | Format, Clippy, all-feature tests, dependency policy | None |
| Push/merge to `main` | The same checks, then a Linux amd64 release build | Automatically deploy that build to staging and check `/healthz` |
| Annotated `vX.Y.Z` tag on a validated `main` commit | Verify the exact commit passed CI and staging; use its existing build | Create a GitHub Release and automatically deploy that build to production |

The tag is the human production release decision. The production workflow must
not rebuild from the tag: it should fetch the artifact produced for the same
commit that ran on staging, verify its checksum, and deploy it. Record the
commit, artifact checksum, deployment result, and previous version. Reject tags
pointing outside `main`, tags without a successful staging deployment, and tags
whose artifact has expired. Staging artifacts currently expire after 30 days.
Tags should not be moved or deleted. Use GitHub tag
rulesets and deployment environments when the repository plan permits them.

Production deployment is **not configured yet**. Before replacing the running
Go relay, verify the production host architecture, SSH access, cert paths,
database migration/backup plan, health check, and rollback. Then add a
production deploy credential and workflow; do not reuse the staging key. A
rollback workflow should redeploy a previously released artifact by checksum.

## Current staging deployment

The `CI` workflow runs all four Rust gates for pull requests and pushes. A
successful push to `main` builds a static Linux amd64 binary on a GitHub-hosted
runner. It copies the binary to a dedicated `vorp-deploy` account over SSH and
invokes `/usr/local/sbin/vorp-deploy-staging` through a narrowly scoped sudo
rule. The server verifies the checksum, replaces `/opt/vorp/vorp`, restarts
`vorp.service`, and checks the HTTPS health endpoint. On failure it restores
the previous binary and restarts the service. The SQLite database and TLS
files are outside this release path.

Repository configuration:

- `STAGING_SSH_KEY` (secret): private key for the dedicated deploy account.
- `STAGING_SSH_HOST` (variable): `vorp-staging.themidst.xyz`.
- `STAGING_SSH_KNOWN_HOSTS` (variable): pinned ED25519 host key, checked against
  the staging host before adding it.

The matching public key is restricted from forwarding and interactive TTY use.
Only the deploy script is permitted through sudo. If the EC2 public address
changes, update DNS; the workflow connects by hostname. GitHub-hosted runner
source IPs change, so staging SSH must be reachable from those runners.
