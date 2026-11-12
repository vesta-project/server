# Download-only Fabric/Quilt server bundles (companion launcher)

Date: **2026-10-06**. Provider-plan step 4. The authoritative implementation record is `launcher-server-mvp/docs/archive/FABRIC_QUILT_SERVER_BUNDLE_CHECKPOINT.md`.

The companion launcher's backend now installs **Fabric** and **Quilt** dedicated servers by downloading verified files (Vanilla + loader + libraries) into a schema-2 multi-file plan and receipt, with **no installer execution** and no EULA acceptance. Library membership comes from the same Modrinth catalogue/profile the launcher already uses, must equal the official Fabric/Quilt server profile, and is bound into the reviewed plan. Digests come from official metadata or official Maven SHA-256 sidecars, with sizes from official data or a bounded HEAD length; Quilt meta/Maven disagreement fails closed. Main classes, game-jar properties and the generated `@argfile` launch are compiled policy (Java 9+). The frontend supplies only software and version choices. Forge/NeoForge remain unavailable.

Software support and a verified bundle grant **no management authority**; Microsoft/Minecraft identity remains the intended common online-mode flow.

Fresh evidence: full `piston-lib` suite 367 passed (three consecutive runs), 19 new parent-authored loader tests, six executed and restored guard mutations, production check, rustfmt and 146 Node/ledger tests. A complete independent read-only review found four defects in total (Java-8 argfile floor, experimental loader opt-in, schema-2 hash domain, workspace path tightening), all fixed with regression tests; the follow-up suite is 372 passed with twelve executed and restored guard mutations across both commits.

No requirement or phase status is promoted (ledger unchanged: 370 entries, 59 partially verified, 311 not verified). Hosting UI/headless/config adapters, managed Java, authenticated remote installation, durable operation integration, native confinement, the independent supervisor and signed distribution remain open.
