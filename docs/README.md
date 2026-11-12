# Documentation

## Product and development

- [Server requirements](SERVER_SOFTWARE_REQUIREMENTS.md)
- [Implementation plan](SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md)
- [Feature roadmap](SERVER_SOFTWARE_ROADMAP.html)
- [Run the agent](../RUNNING.md) and [test/merge gates](../TESTING.md)
- [Development shell](operations/dev-session.md)

## Host operations

- [Guided host setup](operations/host-setup.md)
- [Server-scoped CLI](operations/scoped-cli.md)
- [Supervisor ownership and recovery](operations/supervisor.md)
- [Management API](operations/management-api-contract.md)
- [Signed installation packaging](operations/bootstrap-release.md)
- [Nightly releases](operations/nightly-releases.md)
- [Signed update trust and staging](operations/signed-updates.md)
- [Uninstall and recovery](operations/uninstall.md)
- [Worlds](operations/world-management.md), [server software](operations/software-management.md), and [SFTP](operations/sftp.md)

## Evidence and history

- `reviews/` records scoped findings and checks actually run; reports are dated
  snapshots, not substitutes for current CI results.
- `archive/` preserves earlier design and migration checkpoints.
- `RUST_WRAPPER_PARITY.json`, `JAVA_NETWORK_SURFACE.md` and
  `MIGRATION_GAP_MAP_AND_TIMELINE.md` preserve historical Java migration evidence.
  [Repository ownership and optional comparison](operations/repository-split.md)
  explain their scope. Java source, connector builds and legacy wrapper deployment
  belong to the independent Java repository.
