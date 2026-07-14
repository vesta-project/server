# Publishing Vesta Server Sync

## Products

| Artifact | Gradle target | Who needs it |
|----------|---------------|--------------|
| **Wrapper** (primary) | `:wrapper:installDist` | Every server host — MC-agnostic sync sidecar |
| **Mod release-line JARs** (optional) | `:fabric-1.21:build`, etc. | Mod/plugin-only installs; future auth and join control |

The wrapper is the main distribution. Mod JARs are tiered by **loader + MC generation** (not per patch). See [`versions-matrix.toml`](../versions-matrix.toml).

## Build wrapper (always)

```powershell
cd V:\server
.\gradlew.bat :core:test :wrapper:installDist
```

Output: `wrapper\build\install\wrapper\`

## Build mod release lines

All release lines are defined in [`versions-matrix.toml`](../versions-matrix.toml). Generated `versions/{id}/gradle.properties` files are created at Gradle sync and are gitignored.

Build every release line:

```powershell
.\gradlew.bat build
```

Build one line (active dev target is `fabric-1.21` in `stonecutter.gradle.kts`):

```powershell
.\gradlew.bat :fabric-1.21:build
```

JAR naming:

```
vesta-sync-{modVersion}+{releaseId}.jar
# e.g. vesta-sync-0.1.0-alpha.1+fabric-1.21.jar
```

Each JAR declares a supported MC **range** in mod metadata (e.g. 1.21.1–1.21.4), compiled against the lowest patch in that generation.

## Adding MC support

| Scenario | Action |
|----------|--------|
| New patch in same generation (e.g. 1.21.5) | Widen `mc_range_max` in `versions-matrix.toml`; re-test existing JAR |
| New MC generation (e.g. 1.22) | Add one `[[release]]` row per loader line |
| New loader | Add buildscript + release rows per generation |

## CI recommendation

1. `:core:test` — always
2. `:wrapper:installDist` — every release
3. Mod `:build` — when `src/` or `versions-matrix.toml` changes, or on tagged releases
