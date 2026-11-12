# Source-build warning results

CraftBukkit and Spigot reference hashes are now a reproducibility observation,
not a requirement that every locally compiled JAR be byte-identical to upstream.
This follows the user's approved policy. It changes no downloaded-artifact hash
check, approved source commit, BuildTools pin, process limit, incomplete-compiler
failure, stopped hold or generated bootstrap/library validation.

The source plan hashes its warning policy. The receipt stores expected and actual
hashes and derives `source_build_reference_mismatch` from them. Reverification
requires exactly the derived warnings, then rehashes the actual installed JAR.
Hiding the warning or changing local bytes fails. Rollback uses retained local
artifact evidence.

Software jobs remain `succeeded`, with an additive `warnings` array. Small result
records use a service-owned SQLite companion table so active-job polling does
not load large private launch documents. Warnings survive service reopen and
idempotent job reads. Existing jobs and older agents default to an empty array.
The launcher uses the existing outcome panel, showing “Installed … with a
warning” and the specific explanation. No extra confirmation or modal is added.

## Validation

- Windows installer: 205 passed, two opt-in diagnostics ignored. Tests include
  matching and mismatching receipts for both targets, required warning presence,
  and rejection of later JAR tampering. Download mismatch tests remain green.
- Software service: 15 scoped tests passed, including the small-result-table
  persistence and idempotency tests. Strict installer/control Clippy passes.
- Launcher typechecking and 18 hosting/localization checks passed. Native client
  tests cover warning round trips and old agents without a warnings field
  (five client tests passed). Full workspace compilation also passed.
- A fresh real Ubuntu Spigot 26.3 build installed and re-verified successfully.
  Receipt warnings contain `source_build`, `first_run_effects` and
  `source_build_reference_mismatch`. Reference:
  `d64fb48abc0a02c09273487248b641005b363c5070860d97bc13a35c8808a4e4`.
  Local output: 85,667,272 bytes,
  `c5a3ad776e222219b7a0aad44173a591fb5757b01b57f4f0aa2354ff08c2df56`.
  Minecraft was not started and no EULA accepted. No live configuration changed.

Logs: `V:/tmp/source-warning-*-20261027.log`. Real test root:
`/home/eatham/.cache/vesta-buildtools-runs-20261027/spigot-26.3-warning`.
