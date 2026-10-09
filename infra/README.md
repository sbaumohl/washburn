# infra/: Vultr VMs for Washburn

OpenTofu creates three role groups of Vultr instances (crawler, data, web) in a
single region. cloud-init installs Docker and Tailscale on each, joins the
tailnet with Tailscale SSH enabled, and disables OpenSSH. Everything after that
(Compose stacks, config, deploys) happens over Tailscale. See PROJECTS.md
sections 1 (D7, D8, D11, D12) and 3.6.

Nothing here commits secrets. `terraform.tfvars.json` is rendered by
`scripts/render-config.py` from `washburn.toml` and is gitignored.

## Prerequisites

1. **OpenTofu** (>= 1.6). Linux standalone install:

   ```sh
   curl -fsSL https://get.opentofu.org/install-opentofu.sh | sh -s -- --install-method standalone --install-path $HOME/.local/bin --symlink-path -
   ```

   Verified with OpenTofu 1.13.1 (linux_amd64). Make sure `$HOME/.local/bin` is on `PATH`.

2. **Vultr API key.** Account settings > API. Put it in `[vultr].api_key`.
   Your client IP must be in the API access control list, or every call fails.

3. **Uploaded SSH key.** Upload a public key in the Vultr console or with
   `vultr-cli ssh-key create`, then copy its ID (`vultr-cli ssh-key list`) into
   `[vultr].ssh_key_ids`. SSH is disabled on every VM, so this key is only a
   break-glass credential that Vultr installs. Recovery uses the Vultr web console.

4. **Tailscale pre-auth key.** Admin console > Settings > Keys > Generate auth key.
   Tick **Reusable** and **Ephemeral**, and set an expiry. All three VMs use the same
   key, so it must be reusable. Ephemeral keys make nodes expire from the tailnet
   when they go offline. Put it in `[tailscale].auth_key`.
   Consequence: an ephemeral node is removed from the tailnet after a prolonged outage.
   If a VM is down long enough to be removed, it will not rejoin on its own. Re-run
   `tailscale up` with a fresh key from the Vultr web console.

5. **Tailnet name.** Admin console > DNS. This is the MagicDNS suffix, e.g.
   `tail1234.ts.net`. Put it in `[tailscale].tailnet`. MagicDNS must be enabled.

6. **Ubuntu 24.04 OS ID.** `os_id = 2284` is Ubuntu 24.04 LTS x64 in the example TOML.
   Verify with `vultr-cli os list`. The cloud-init assumes noble, so do not use another OS.

## Contract: `infra/terraform.tfvars.json`

`scripts/render-config.py` (W7) must emit this file. Every key is required except
those marked optional. Types are exact.

| Variable | Type | Source in washburn.toml | Notes |
|---|---|---|---|
| `vultr_api_key` | string (sensitive) | `[vultr].api_key` | |
| `region` | string | `[vultr].region` | |
| `os_id` | number | `[vultr].os_id` | Ubuntu 24.04 |
| `ssh_key_ids` | list(string) | `[vultr].ssh_key_ids` | |
| `plans` | object `{crawler, data, web}` of string | `[vultr.plans]` | |
| `counts` | object `{crawler}` of number | `[vultr.counts]` | |
| `tailscale_auth_key` | string (sensitive) | `[tailscale].auth_key` | Ends up in user-data (see below) |
| `tailnet` | string | `[tailscale].tailnet` | Used only for output names |
| `public_host` | string | `[domain].public_host` | Used only for `dns_records_needed`. Added beyond the W5 brief. |
| `enable_vpc` | bool, optional, default `false` | not in TOML | Renderer may omit it |
| `project_tag` | string, optional, default `"washburn"` | not in TOML | Renderer may omit it |
| `enable_reserved_ip` | bool, optional, default `false` | not in TOML | Renderer may omit it |

Example (fake values, also at `infra/examples/terraform.tfvars.json`):

```json
{
  "vultr_api_key": "FAKE-VULTR-API-KEY",
  "region": "ewr",
  "os_id": 2284,
  "ssh_key_ids": ["00000000-0000-0000-0000-000000000000"],
  "plans": {
    "crawler": "vc2-2c-4gb",
    "data": "vhp-4c-8gb-amd",
    "web": "vc2-2c-4gb"
  },
  "counts": {
    "crawler": 1
  },
  "tailscale_auth_key": "tskey-auth-FAKE",
  "tailnet": "tail0000.ts.net",
  "public_host": "example.com",
  "enable_vpc": false,
  "project_tag": "washburn",
  "enable_reserved_ip": false
}
```

## Commands

Run from `infra/`. Put the real `terraform.tfvars.json` here, or pass a path.

```sh
tofu init                                        # downloads vultr/vultr ~> 2.21, honours .terraform.lock.hcl
tofu fmt -check -recursive                       # formatting
tofu validate                                    # schema and template check, no credentials needed
tofu plan -var-file=terraform.tfvars.json        # needs the Vultr key; creates do not call the API at plan time
tofu apply -var-file=terraform.tfvars.json
tofu output                                      # web IPs, instance IDs, tailnet names, DNS instructions
```

`.terraform.lock.hcl` IS committed. It pins the provider version and hashes. The
init run that produced it recorded hashes only for the current platform. To add
hashes for other platforms, run `tofu providers lock -platform=linux_amd64 -platform=darwin_arm64`
(adjust the list to your machines).

The state file (`terraform.tfstate`) is local and gitignored. It contains the
Tailscale key and the rendered user-data, so treat it as secret.

## What cloud-init does

One template, `cloud-init/common.yaml.tftpl`, with a `role` variable. The bootstrap
script runs with `set -euo pipefail` and in this order:

1. Set hostname, install Docker CE and the compose plugin from Docker's apt repo.
2. Install Tailscale from `pkgs.tailscale.com`, then
   `tailscale up --authkey=... --hostname=<name> --ssh`.
3. Create `/opt/washburn` and `/etc/washburn` (0750) and write `/etc/washburn/role`.
4. Data only: `/opt/washburn/pgdata` (owned 999:999 for the postgres image), `vm.swappiness=10`.
5. Set password auth off. Stop, disable and mask `ssh.service` and `ssh.socket`.

Step 5 runs only after step 2 succeeds, so a failed join leaves sshd running.
Recovery in that case: open the Vultr web console, read
`/var/log/cloud-init-output.log`, and if needed run `systemctl unmask ssh.socket ssh.service`.

**Secret exposure.** The Tailscale auth key is in user-data, as PROJECTS.md 3.6
allows. D8 as written says secrets never go into user-data, so this is a deliberate
exception. Consequences: user-data is readable from the instance metadata endpoint
by anything on the VM, it is in `/var/lib/cloud`, and it is in the tofu state. This
is why the key must be ephemeral and expiring. Rotate it after the first deploy.

cloud-init runs once per instance. Editing the template does not reconfigure existing VMs.

## After apply

1. **DNS (manual).** `tofu output dns_records_needed` prints the A and AAAA targets.
   Create A `<public_host>` -> `web_public_ipv4` and AAAA `<public_host>` -> `web_public_ipv6`.
   The admin host gets no public record. Without `enable_reserved_ip`, the IPv4
   can change if the web VM is re-created, so enable it before go-live.

2. **Confirm the nodes joined the tailnet.** `tailscale status` on your laptop should
   list `crawler-1`, `data` and `web`. Names must match `tofu output tailscale_names`.
   If an old node with the same name is still listed, the new one becomes
   `data-1` and the names diverge. Remove the stale node in the admin console.

3. **Tailscale ACL.** Nodes join with `--advertise-tags=tag:washburn,tag:washburn-<role>`,
   so the policy refers to tags, never hostnames (hostnames are not valid ACL
   destinations). In the admin console, Access Controls, merge this in. If you
   remove the default `autogroup:member -> *` rule, the first `acls` entry keeps
   your own devices talking to each other.

   ```jsonc
   {
     "tagOwners": {
       "tag:washburn":         ["autogroup:admin"],
       "tag:washburn-crawler": ["autogroup:admin"],
       "tag:washburn-data":    ["autogroup:admin"],
       "tag:washburn-web":     ["autogroup:admin"],
     },
     "acls": [
       { "action": "accept", "src": ["autogroup:member"], "dst": ["autogroup:self:*"] },
       { "action": "accept", "src": ["autogroup:admin"],  "dst": ["tag:washburn:*"] },
       { "action": "accept", "src": ["tag:washburn-crawler", "tag:washburn-web"],
         "dst": ["tag:washburn-data:5432"] },
     ],
     "ssh": [
       { "action": "accept", "src": ["autogroup:admin"], "dst": ["tag:washburn"],
         "users": ["root", "autogroup:nonroot"] },
     ],
   }
   ```

   Nodes that joined before this change (or with an older template) carry no
   tags yet: in Machines, open each of `crawler-1`, `data`, `web`, choose
   Edit ACL tags, and add `tag:washburn` plus its role tag. Tagged nodes do not
   have key expiry, which is what you want for servers. Tailscale SSH needs the
   `ssh` block; without it `tailscale ssh root@data` is refused.

4. **Check Docker.** `tailscale ssh root@web 'docker info --format {{.ServerVersion}}'`.

## Adding a second crawler

Set `[vultr.counts].crawler = 2` in `washburn.toml`, re-run `scripts/render-config.py`,
then `tofu plan` and `tofu apply`. This adds `crawler-2` only. The other VMs are untouched.
It advertises the same tags, so the ACL needs no change. The crawler firewall group is shared,
so there is nothing else to change in the firewall.

## Optional features

- `enable_vpc = true` creates a `vultr_vpc` and attaches every instance to it through
  `vpc_ids`. Nothing downstream may depend on private IPs (D11). This is off by default.
  `vultr_vpc2` is deprecated in provider 2.32, so the module uses `vultr_vpc`.
- `enable_reserved_ip = true` creates a reserved IPv4 attached to `web`. The
  `web_public_ipv4` output then reports the reserved address.

## Provider notes

- Pinned to `~> 2.21`. Resolved to **2.32.0** during verification.
- The provider base64-encodes `user_data` itself, so the template output is passed in plaintext.
- `vpc2_ids` is deprecated and not used. `vpc_ids` is used instead.
- Firewall IPv6 "anywhere" uses `subnet = "::"`, `subnet_size = 0`. The provider's own tests only exercise the IPv4 `0.0.0.0` form, so the IPv6 form is unverified. Confirm with a live check after the first apply (for example, `curl -6` to port 80 of `web_public_ipv6` from outside, and a UDP check on 41641).

## Protecting machines Washburn must never touch

Vultr API keys are account-wide: there is no per-instance scoping and no
deletion-protection flag. Keep your existing machine safe with these rules:

1. **State discipline.** OpenTofu only manages resources in its state file. Your
   existing machine is not in it and must never be `tofu import`ed. Read every
   `tofu plan` before applying; a correct plan for this repo only ever creates
   resources named `crawler-N`, `data`, `web`, two firewall groups, and
   optionally a VPC and a reserved IP.
2. **Deploy allowlist.** `scripts/deploy.sh` accepts only `crawler-N`, `data`
   and `web` as targets, and only when they match the role.
3. **Strongest option: a sub-account.** Vultr sub-accounts are isolated accounts
   billed through the parent, each with its own API key. Run Washburn from a
   sub-account and the key used here cannot see your other machine at all.
   Take a snapshot of that machine before the first apply as cheap insurance.
