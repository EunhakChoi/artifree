# Builds on the supported host

Status: R03fb, implemented ([decision 0058](decisions/0058-react-builds-and-preview.md),
rule 21). **Ubuntu 24.04 on x86_64 is the supported host for builds.** The
steps below were run end to end only by the `Supported host (Ubuntu 24.04
x86_64)` CI job, on GitHub's `ubuntu-24.04` runner, from the `linux-x64`
archive of the same change. That job does not reboot the host, upgrade a
release, or run the server behind a proxy; those steps are described here but
have not been exercised.

A `react-v1` build runs untrusted code: the bundler reads the artifact's
source in a sandbox ([decision 0042](decisions/0042-linux-untrusted-execution.md)).
The server never runs a build itself. A second process, the execution worker,
claims builds and runs each one inside a tool root that you prepare as root,
once for each server release. Without a ready execution worker a build stays
queued and ends as `execution_unavailable` after ten minutes (rule 20).

## What is supported

| Host                                | Builds                                                                                                                                                                                                       |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Ubuntu 24.04, x86_64, this guide    | Supported.                                                                                                                                                                                                   |
| Another x86_64 Linux                | Not supported, and not prevented. The server checks no distribution: the worker's admission checks decide. A host that passes them runs builds, unsupported; one that fails them names the missing facility. |
| The [container image](container.md) | The image holds no tool root, so no execution worker is ready there, and a build ends as `execution_unavailable`.                                                                                            |
| Windows, or Linux on another CPU    | The server knows no build profile and refuses a build with `422 profile_unknown`.                                                                                                                            |

The sandbox's syscall filter and its refusal matrix were reviewed and run on
the hosted runner's kernel, a 7.0 release, by the `Linux enforcement` job
([decision 0056](decisions/0056-linux-provider-delivery.md)). Ubuntu 24.04
keeps the kernel, the launcher's loader and its libraries current with its
security updates. Nothing at admission checks the kernel release, so another
Ubuntu 24.04 kernel runs builds as well, but it was not run.

Everything else in the package (the server, the web workspace, the CLI,
verification jobs, backups) works without this guide; see
[release packages](release-packages.md).

## What the package holds for this

The `linux-x64` archive carries, under `tools/`:

| File                         | Purpose                                                                                                                |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `package-root.sh`            | The packaging script of rule 19: builds `/opt/artifree/tools/<version>` for one server and loads its AppArmor profile. |
| `react-v1.lock`              | The lock list the script installs: the bundler and the React packages, each with its URL, SHA-256 and licence.         |
| `artifree.service`           | The systemd unit of the server.                                                                                        |
| `artifree-execution.service` | The systemd unit of the execution worker, with its own delegated control group (decision 0056 rule 8).                 |

The units assume the paths of this guide: the release under
`/opt/artifree/current`, data in `/var/lib/artifree`, and the account
`artifree`. Change the units if you use others.

## Install

Run these as an administrator on a fresh Ubuntu 24.04 x86_64 machine. They
change system settings, so read each step first.

1. **Check and unpack the archive** into its own directory, and point
   `/opt/artifree/current` at it:

   ```sh
   sha256sum -c artifree-<version>-linux-x64.tar.gz.sha256
   sudo install -d -o root -g root -m 0755 /opt/artifree /opt/artifree/releases
   sudo tar -xzf artifree-<version>-linux-x64.tar.gz -C /opt/artifree/releases --no-same-owner
   sudo ln -sfn /opt/artifree/releases/artifree-<version>-linux-x64 /opt/artifree/current
   ```

2. **Route core dumps to a plain file.** The worker refuses a host that pipes
   core dumps to a program, because a crashing build would then run that
   program with the build's memory. Ubuntu's crash reporter, apport, sets such
   a pipe, so it is turned off, and a late-sorting `sysctl.d` file keeps the
   plain name after a reboot:

   ```sh
   sudo systemctl disable --now apport.service   # if apport is installed
   printf 'kernel.core_pattern = core\n' | sudo tee /etc/sysctl.d/99-artifree-core.conf
   sudo sysctl -p /etc/sysctl.d/99-artifree-core.conf
   cat /proc/sys/kernel/core_pattern             # must print: core
   ```

   If `systemd-coredump` is installed, its own pipe comes from an earlier
   `sysctl.d` file, which the `99-` file overrides.

3. **Keep the parents of the tool root writable by root only.** The root is
   `/opt/artifree/tools/<version>`, and the worker refuses it when `/`, `/opt`,
   `/opt/artifree` or `/opt/artifree/tools` is writable by anyone but root. A
   stock Ubuntu `/opt` is `0755`; some images make it `0777`:

   ```sh
   stat -c '%a %U' /opt    # must print: 755 root
   sudo chmod 0755 /opt    # only if it did not
   ```

4. **Prepare the tool root of this release** with the package's own script and
   server:

   ```sh
   sudo /opt/artifree/current/tools/package-root.sh /opt/artifree/current/bin/artifree-server
   ```

   It installs Bubblewrap `0.9.0-1ubuntu0.3` with apt (and refuses to
   downgrade a newer one), downloads the archives of the lock list and checks
   each SHA-256 before it writes anything, assembles
   `/opt/artifree/tools/<version>` (the version `artifree-server --version`
   names), loads the AppArmor profile `artifree-launcher-<version>` that lets
   the root's launcher create user namespaces, and prints the root's path. It
   refuses a root that already exists: a root is never replaced in place. If
   it fails, it removes what it made, and you can run it again.

5. **Create the account and its directories.** The server and the worker run
   as the same account, on the same data directory. The execution directory
   holds the worker's lock, its attempt registry and the staged input; it must
   be private to the account:

   ```sh
   sudo useradd --system --user-group --home-dir /var/lib/artifree --shell /usr/sbin/nologin artifree
   sudo install -d -o artifree -g artifree -m 0700 /var/lib/artifree /var/lib/artifree/execution
   ```

   The server creates `/var/lib/artifree/data` at its first start.

6. **Install and start the units**, the server first:

   ```sh
   sudo install -o root -g root -m 0644 -t /etc/systemd/system \
     /opt/artifree/current/tools/artifree.service /opt/artifree/current/tools/artifree-execution.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now artifree.service
   curl --fail http://127.0.0.1:7410/healthz      # prints ok once the server is up
   sudo systemctl enable --now artifree-execution.service
   ```

   The execution unit has `Delegate=yes`, and the worker is its own command.
   At start the worker checks that it is the only process of the unit's
   control group and that memory, CPU and task control are delegated to it,
   and moves itself into a child group. Do not wrap the command in a shell or
   another program, and do not add systemd sandboxing settings to this unit:
   the CI job ran it exactly as shipped. The worker opens an existing database
   only, so if it starts before the server has made one, it exits and systemd
   starts it again five seconds later.

7. **Check readiness.** `GET /v1/background` reports `execution.ready: true`
   once an execution worker was seen in the last five minutes; the Jobs tab of
   the web workspace says so too:

   ```sh
   sudo install -o "$USER" -m 0600 /var/lib/artifree/data/access-token ~/artifree-token
   curl --fail -H "Authorization: Bearer $(cat ~/artifree-token)" http://127.0.0.1:7410/v1/background
   ```

   If it stays `false`, read the worker's journal
   (`journalctl -u artifree-execution.service`). A refused execution
   directory or control group stops the worker at start with status 2 and one
   line that says why. A host that fails admission (the core routing, the
   root, a root ancestor, the namespaces) keeps the worker running, but it
   claims no build, and it writes why to its journal once for each run of
   identical refusals.

8. **Build something.** With the packaged CLI (Node.js 22.12 or newer), from
   a directory with an `index.html` that loads `./app.js` and a `src/main.tsx`:

   ```sh
   export ARTIFREE_TOKEN_FILE=~/artifree-token
   node /opt/artifree/current/cli/artifree.mjs artifact create --title Demo --dir ./site
   node /opt/artifree/current/cli/artifree.mjs job enqueue <id> --revision 1 --build react-v1
   node /opt/artifree/current/cli/artifree.mjs job show <job-id> --wait
   node /opt/artifree/current/cli/artifree.mjs artifact preview <id> --build react-v1
   ```

   The preview URL is on the content origin's `/b/` route and serves the
   build: `index.html`, `app.js` and `app.css`.

## Upgrade and remove an old root

Each server release names a new root, because the root holds that server as
its bootstrap. An upgrade therefore prepares the new root beside the old one,
switches both units over, and removes the old root last:

1. Take a backup ([release packages](release-packages.md#upgrade-in-place)).
2. Unpack the new archive into `/opt/artifree/releases` (step 1, without the
   link) and prepare its root with its own script and server (step 4). The
   two roots and their AppArmor profiles sit side by side.
3. Stop both units, the execution worker first:
   `sudo systemctl stop artifree-execution.service artifree.service`.
4. Point `/opt/artifree/current` at the new release, compare the new
   package's units with the installed ones, and copy them if they changed
   (then `sudo systemctl daemon-reload`).
5. Start the server, then the worker (step 6), and check readiness (step 7).
6. When you are satisfied, and no unit runs the old release any more, remove
   the old root and its launcher profile:

   ```sh
   sudo apparmor_parser -R /etc/apparmor.d/artifree-launcher-<old-version>
   sudo rm /etc/apparmor.d/artifree-launcher-<old-version>
   sudo rm -rf /opt/artifree/tools/<old-version>
   ```

   The enforcement CI job removes a root the same way before it packages a
   new one; the upgrade itself has not been run.

To go back to the old release before step 6, stop both units, point the link
back and start them again; its root is still there. A database the new
release has upgraded is refused by the old one, so restore the backup first.

## What CI checks

The `Supported host (Ubuntu 24.04 x86_64)` job (`scripts/verify-supported-host.sh`)
downloads the `linux-x64` archive that the `Release archive` job built, and on a
fresh runner, with a sparse checkout of `scripts/` and the bundler template and
no build, it follows steps 1 to 7, with the
archive's script and units unchanged. It then creates an artifact from the
bundler template with the packaged CLI, builds revision 1 with `react-v1`,
requires the job to succeed with three files, the head status to be `built`,
and the build preview under `/b/` to serve the template's page, a bundle that
holds the template's code and React, and the style sheet. It also requires that
systemd never had to restart the worker.

The `Clean install` job checks the archives' entry points: the `linux-x64`
server serves `react-v1`'s entry points, fails the fixture profile's identity
as unknown, and reports a version that names a root, and the `windows-x64`
server refuses every entry point. The `Linux enforcement` job runs the full
refusal matrix of the sandbox, including a root packaged by the same script for
the server built as the archives build it.
