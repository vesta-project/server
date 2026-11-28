# Vesta Server: install, setup and connections

Status: accepted for a first implementation pass, 2026-11-13. Expect revisions as phases hit problems.
Reviews folded in: Sol (r2, r5), Fable 5.2 (r5).
Scope:
- server repo: `vesta-setup`, `vesta-agent`, `vesta-control`, `vesta-transport`
- launcher: `apps/desktop-solid`, `vesta-launcher/src-tauri/src/servers`

## Principles

1. **Self-hosted first.** Nothing Vesta hosts is ever required. Everything works on your LAN and through your own port forwards. Hosted extras are optional, can be swapped for your own, and nothing breaks if they disappear.
2. **People type what they already know.** Players type a Minecraft address. Owners click a button, or type their machine's address.
3. **Invites are the main way to share.** They carry everything, so nobody guesses ports.
4. **Game traffic is never proxied.** Players connect straight to Minecraft, so real IPs and IP bans keep working.
5. **Sensible defaults everywhere.** Settings stay behind "Customize".
6. **Copy never says** "agent", "host root", "activate" or "pending".

## Decisions

| # | Topic | Decision |
|---|---|---|
| 1 | Name | **Vesta Server** |
| 2 | Process model | A main Vesta Server per machine, with a child worker process per Minecraft server. Later, other PCs can join a main machine (§9). |
| 3 | Migration | None (pre-release). The per-server listener is replaced outright. |
| 4 | LAN discovery | mDNS `_vesta._tcp` |
| 5 | Bind | LAN by default. Port forwarding is supported and helped. |
| 6 | Hosted services | Optional only: reachability probe, possibly a self-hostable relay later |
| 7 | Management port | **25500** by default, any port allowed |
| 8 | Game traffic | Direct, never proxied |
| 9 | Permissions | Primary owner, owners and per-server roles (§6) |
| 10 | A stranger types a set-up server's address | It's added to their Servers list as a normal server, with no permissions and no management prompt |

## 1. Why the current model is messy

Today every Minecraft server runs its own `vesta-agent serve` with its own HTTPS port, TLS identity, devices and pairing. That means:
- N ports to open and N pairings
- install, activate, create and Java as separate CLI steps
- an onboarding endpoint that only listens on loopback
- no first-server setup
- auto-start that only runs when the user logs in (`vesta-setup/src/platform.rs`), so a reboot from Windows Update leaves the server down

## 2. The model

```
Launcher ──► Main Vesta Server  (one per machine, one management port, one identity)
               ├── accounts, devices, roles, invites, registry, Java + download cache
               ├── worker ── Survival   (game port 25565, players connect directly)
               ├── worker ── Modded     (game port 25566)
               └── later: joined PC "Gaming rig" ── worker ── Creative   (§9)
```

The main Vesta Server owns:
- the management listener and router: `/v1/host/*`, `/v1/here`, `/v1/servers/{id}/*`
- identity and roles
- setup, linking and invites
- mDNS, firewall and UPnP
- updates and the server registry
- shared Java and downloads
- the single SFTP listener
- worker supervision

Workers run one Minecraft server each: files, packs, console, sleep, backups.

## 3. Owner journeys

### 3.1 Install on the server machine (the normal case)

Vesta Server is meant for a **separate machine**: a spare PC on the LAN (most owners) or a VPS. The launcher runs on the owner's own PC. Running both on one PC works, but it's not a case we design around.

On the server machine, one command:

```powershell
irm https://<vesta-site>/install | iex          # Windows
curl -fsSL https://<vesta-site>/install | sh    # Linux
# optional: -Port 25500  -ServerDir "D:\mc\survival"  -LocalOnly
```

The script:
- verifies the signed bundle with pinned keys
- installs the boot service under its own account
- adds firewall rules for Vesta and Java
- binds to the LAN and advertises over mDNS
- puts `vesta` on PATH

It prints:

```
Vesta Server is running on SPARE-PC (192.168.1.67:25500)
  On your PC, open the launcher: Servers → Set up a Vesta Server
  Setup code: K7Q4-MX29-P3     (new code any time: vesta setup-code)
```

### 3.2 Claim from the launcher

1. Servers → **Set up a Vesta Server**. Unclaimed machines on the LAN are listed ("SPARE-PC, ready to set up"), and are picked with one click. For a VPS or across networks, type the IP or domain. The port is only needed if it isn't 25500.
2. Enter the setup code.
3. Microsoft sign-in. The account becomes **primary owner**, and the device is linked.
4. **First server wizard** (§3.4).

The setup code: is the password for a PAKE (CPace or SPAKE2) run inside TLS and bound to the session with the TLS exporter. So the certificate is proven without a trust prompt, and someone in the middle gets one guess per attempt.
- Codes expire after 30 minutes, are single-use and rate limited. `vesta setup-code` makes a new one at any time while the machine is unclaimed.

### 3.3 Adding more devices for the owner

From an owner device: Host settings → **Link a device** shows a code. On the machine itself, `vesta link` prints one.
- The new device enters the code (Add server → **I have a link code**). On the LAN the machine is already listed, so only the code is typed. QR linking comes with mobile support.
- It signs a link request covering host id, device key, Microsoft account, role, a nonce and an expiry. The code authenticates it through the same PAKE.
- The owner device confirms "Link Ethan's laptop?".
- If no owner device is available, `vesta link --approve` on the machine approves it. This needs the local control channel with OS admin rights.

### 3.4 First server wizard

Choose one:
- **New:** vanilla or a loader
- **Modpack:** the Browse picker, targeting the server
- **Import a folder:** `-ServerDir` is preselected. The folder is copied, the original is left untouched, and port and running-process conflicts are resolved first.
- **Restore a backup**

Defaults are filled in and the only required action is **Create**:
- name and latest version
- game port: next free from 25565
- memory: from a **shared machine budget**, so five servers don't each claim "recommended"
- EULA: an inline checkbox
- Java: picked and downloaded automatically

**Customize** reveals the rest.

The end screen shows:
- **Copy invite**, the primary action
- the LAN join address
- **Open to friends** (§7)

### 3.5 Experienced owners

Everything is scriptable on the machine and host-scoped, with no `--install-root` or `--java` unless overriding:
- `vesta status | setup-code | link | devices | update | uninstall`
- `vesta server list | create | import | start | stop | console | logs | delete`

Headless installs use an answers file plus `--no-start` / `--no-service`.

## 4. Player and staff journeys

### 4.1 Invite (the main path)

An invite is a link or a short code. It carries:
- every known endpoint (LAN and public), with the real external port
- host id and fingerprint
- serverId and an invite code
- the pack reference

Accepting it is **one flow**: sign in if needed, then the server is added, its role is accepted, and the pack is installed into a linked instance, ready to Play. Expired or used invites say so and suggest asking the owner for a new one.

The owner's **Copy invite** shows whether the invite's endpoint has been proven reachable from outside (§7).

### 4.2 Typed Minecraft address

The launcher resolves it like Minecraft does:
- SRV only when no port was typed, honouring priority and weight
- IPv6 written `[addr]:port`
- every A/AAAA address tried
- the hostname as typed kept for the handshake

It pings the server and shows the **normal server card** right away.

Then, asynchronously with a 3 s cap and cached misses, it looks for a Vesta Server: same host on 25500, then `_vesta._tcp.<domain>`, then mDNS on the LAN. If one answers, the launcher asks `/v1/here?port=<game port>`:
- the answer comes only from a server **bound to that exact game port** and marked public, never from matching status text
- a pack is offered only if it's a **Modrinth or CurseForge reference**. The card shows "Pack: X from Modrinth", and installing it is an explicit click.
- custom packs, unattended pack updates and anything else need an invite

If nothing answers, it's a plain server card, which is the normal Minecraft experience.

### 4.3 Typed Vesta address

`host[:port]`, or a domain with `_vesta._tcp`. This connects to the machine:
- unclaimed: setup (§3.2)
- an owner: every server
- staff: the servers they hold a role on
- anyone else: "You don't have access to this Vesta Server"

### 4.4 Already-linked devices

If a linked device types a Minecraft address that belongs to a machine it knows (same host identity, from `/v1/here`), it shows the managed card. There's no prompt.

## 5. Security

- **Claim:** PAKE setup code from the install output. Claiming is atomic and single-use.
- **Microsoft sign-in:** to be specified before phase 1: token audience and issuer, replay protection, device binding, account id used, and offline behaviour. An arbitrary Microsoft token is never accepted.
- **Linking:** signed requests bound to host id, device key, account, role, nonce and expiry. Unlinking revokes sessions at once on the main server and every worker.
- **Recovery:** `vesta link --recover` only over a privileged local channel that **workers and Minecraft processes can't reach**. It's audited and offers to revoke all other devices. Certificate rotation is separate (`vesta tls rotate`, signed by the old certificate).
- **Isolation:** anyone who can upload mods or plugins can run code. So:
  - each server runs under its own low-privilege OS account where the OS allows it
  - the main server's database, IPC secrets and other servers' files aren't readable by workers
  - where per-server accounts aren't possible, the UI says plainly that server Admins can affect other servers on the machine
  - the promote-to-owner and grant-Admin dialogs explain this
- **Anonymous surface:** `/v1/here`, claim (only while unclaimed), link-code redemption and invite redemption. All are rate limited, and misses look the same whether or not the server exists. Claim state is only shown to LAN requests. Fingerprints from discovery are hints, never trust anchors.

## 6. Permissions

| Level | Who | Can | Sees |
|---|---|---|---|
| **Primary owner** | the account that claimed the machine | everything, including promoting and demoting owners. Can't be demoted, unlinked or revoked by anyone else. | every server |
| **Owner** | promoted by the primary owner | everything except changing owners | every server, including ones added later |
| **Admin** (per server) | role invite | start/stop, console, files, packs, backups, players | servers they hold a role on |
| **Moderator** (per server) | role invite | specific actions: kick, ban, whitelist, restart, read console. Not raw console commands. | servers they hold a role on |
| **Player** (per server) | invite, or public | pack sync, wake a sleeping server | servers they've added |

Primary ownership moves only by transfer (confirmed on the primary owner's device) or by local recovery. Owner checks happen on the main server. Per-server roles are rechecked by the worker when an operation runs.

## 7. Networking (self-hosted)

- **One machine identity, many addresses.** Linked devices store each machine by host id and fingerprint, with all known endpoints: LAN (re-resolved over mDNS when DHCP changes the IP), public IP or domain, and any tunnel address. This also handles routers that can't reach their own public address from inside.
- **Open to friends:** one button.
  1. It tries UPnP or NAT-PMP for the game port and the management port. If 25500 is taken outside, UPnP picks another port and Vesta records the real external one.
  2. If UPnP isn't available, it shows router steps with the actual internal IP and ports filled in.
  3. It then checks reachability.
- **Reachability, three facts never merged:**

  | Fact | How it's known |
  |---|---|
  | Running | local check |
  | Reachable from this launcher | direct connect |
  | Reachable from outside | a friend's launcher reaching the server through an invite counts as proof ("Friends reached this 2 h ago"). There's also an **optional** probe at a configurable URL, which can be Vesta-hosted or your own, and is checked per IPv4 and IPv6. |

  With neither, the launcher says "not checked yet".
- **Changing IPs:** owners can set their own domain or a free dynamic DNS name. Invites carry it, and with a domain, ACME certificates are available.
- **CGNAT:** bring your own tunnel (Tailscale, ZeroTier, WireGuard) and Vesta uses its address. Without one, the launcher says plainly that friends outside can't reach you.
- **Two machines behind one router:** each has its own external port, and invites, mDNS and linked devices carry the real port. Joined PCs (§9) reduce this to one forwarded management port.
- **Windows specifics:**
  - firewall rules for Vesta *and* each Java runtime, so the first-start popup never appears
  - a check for the network profile ("Public" networks block inbound traffic and mDNS)
  - a warning when Wi-Fi client isolation blocks LAN discovery

## 8. Service and install

- Boot-time service: a Windows service, or a system-level systemd unit. Not per-user login autostart.
- A dedicated low-privilege service account, never LocalSystem. Per-server accounts where possible (§5).
- `vesta` on PATH. Standard install locations, with `--root` overridable.
- The one-command script is the install path for spare PCs and VPSs alike, and the only option the website and docs show.
- A Windows .exe that runs the same steps is also published, as a quiet alternative linked from the install docs.

## 9. Joined PCs (later phase)

AMP-style: one main Vesta Server, with other PCs joined to it.
- Main machine → Host settings → **Add another PC** gives a join code. That PC runs the install command and enters the code. It joins as a node, and the two machines authenticate each other with mutual TLS.
- The launcher only talks to the main machine, which forwards management requests to nodes. That means one forwarded port in total, one list of servers, one set of roles and invites.
- Game traffic still goes straight to each machine's game port.
- If the main machine is off, nodes' Minecraft servers keep running but can't be managed. Promoting a node to main comes later.
- **Phase 1 has to allow for this:** the main-server-to-worker contract must work over local IPC *and* over an authenticated network link.

## 10. Process model and worker refactor

`vesta-agent worker <server.toml> --ipc <pipe>`, one per Minecraft server.

This is a security refactor, not "serve minus TLS":
- `serve.rs` passes `ManagementAccess`, the certificate fingerprint and the public endpoint into local control, and it starts its own SFTP
- `vesta-control/src/host.rs` composes identity, management, the database, runtime and authorization together
- transport handlers use server-scoped subjects and management ids

Work needed:
- IPC authenticated by a per-worker secret, on a pipe or socket only the server's account and the main server can open
- subjects issued by the main server and bound to a server id, with authorization rechecked in the worker
- grants and revocations pushed with a generation number, so workers refuse stale subjects
- leases, supervision and shutdown ownership preserved
- SFTP moved into the main server

`vesta-setup` shrinks to the bootstrap. `activate`, the loopback pending endpoint and the per-server listener are removed.

## 11. Launcher changes

- **Store:** `VestaHost { id, name, fingerprint, endpoints[], credentials, authRevision }`, plus server refs `{ hostId, serverId }`. A typed-address server is a saved server with an optional Vesta ref, so plain, Modrinth and Vesta servers share one card model (unified-servers design).
- **Servers tab:**
  - **Set up a Vesta Server:** the LAN list of unclaimed machines, or type an address
  - **Add server:** one box (Minecraft or Vesta address), the LAN list, and I have a code or invite
- **Invites:** one-step accept flow. **Copy invite** on every owned server card and at the end of the wizard.
- **Owner flows:**
  - first-server wizard and New server on…
  - Link a device
  - Open to friends
  - Host settings: endpoints, port, domain/DDNS, tunnel address, probe URL, Java, storage, SFTP, devices, owners
- **Connections:** one realtime connection per host, fanned out to its servers. Machine grouping appears only with 2+ machines.

## 12. Phases

| Phase | Work | Done when |
|---|---|---|
| 0 | Final review of this doc. Freeze in `protocol/`: router, worker link (IPC and network), claim/link, roles, invite format, `/v1/here`, Microsoft token verification. | Signed off |
| 1 | Main server process, router, one worker over authenticated IPC, identity and roles, local-channel claim, PAKE claim, signed link, recovery. A minimal launcher client. Loopback only. | Claim and link work from the launcher, and today's features work through the main server. Tests cover cross-server denial, revocation, stale subjects and restarts. |
| 2 | Many workers, auto game ports, shared memory budget, shared Java and cache, central SFTP, per-server OS accounts | Two servers isolated, with owners seeing a newly added third server automatically |
| 3 | Install script, boot service, service account, firewall (Vesta + Java), network profile check, LAN bind, mDNS, **Set up a Vesta Server** in the launcher | Fresh Windows and Linux machines: one command on the machine, then one click and a code on another PC, end in a claimed machine. Survives a reboot without anyone logging in. |
| 4 | Launcher host store with endpoints, Add server box, invites first (create, copy, one-step accept), typed-address lookup with port binding and registry-only packs | Invite → Play works for a friend. Typed IP shows the plain or pack card per §4.2. |
| 5 | First-server wizard (new, modpack, import, restore), defaults, Customize, auto Java, EULA | A new owner reaches "Server online" and Copy invite without changing a setting |
| 6 | Open to friends (UPnP/NAT-PMP, router steps), reachability evidence, optional probe URL, domain/DDNS, ACME, CLI parity | Correct three facts on a port-forwarded test network |
| 7 | Joined PCs | Two PCs under one main machine, one forwarded port, one server list |
| Later | Optional self-hostable relay as an open protocol, for CGNAT without a tunnel | Not scheduled |

## 13. Decide during implementation (defaults shown)

- Resolved: an .exe ships, but only the script is shown. QR codes wait for mobile support.
- Moderator actions default to kick, ban, whitelist, restart and read console.
- Joined PCs stay at phase 7 unless two-machine owners hit problems first.
