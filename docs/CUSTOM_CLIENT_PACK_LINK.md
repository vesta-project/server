# Provider-linked custom client packs

The existing pack/draft/import management route accepts two additional requests:

- source=provider, expected_revision, provider, project_id, version_id: resolve a verified
  provider archive and replace the draft's provider-owned entries and override files.
- source=unlink, expected_revision, choice: keep_everything, keep_resources, or remove_everything.

The revisioned manifest's optional modpack_link records source IDs and owned entry IDs /
override root-path pairs. Ordinary draft and file writes cannot alter that ownership or those
files. User additions survive version changes and every unlink choice. Path collisions refuse
instead of replacing additions. Runtime compatibility, authorisation, durable intent replay and
revision checks use the existing workspace pipeline. No server game files are modified.

Publishing remains explicit and uses custom pack review/build. The existing Vesta index
references resolvable provider resources by project/version ID, and hosts custom uploads and
overrides. A whole-pack provider pointer would discard the custom additions, so it is not used.
The server-modpack link and client mode remain independent.
