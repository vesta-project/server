use super::*;

fn scan_of(entries: &[(&str, u64)]) -> Files {
    entries
        .iter()
        .map(|(path, len)| ((*path).to_owned(), (*len, 1)))
        .collect()
}

#[test]
fn world_changes_are_classified_by_level_dat_and_volatile_files_are_not_world() {
    let before = scan_of(&[
        ("world/level.dat", 10),
        ("world/region/r.0.0.mca", 100),
        ("logs/latest.log", 5),
        ("server.properties", 3),
    ]);
    let after = scan_of(&[
        ("world/level.dat", 10),
        ("world/region/r.0.0.mca", 101),
        ("logs/latest.log", 9),
        ("server.properties", 3),
        ("world_nether/level.dat", 1),
    ]);
    let changes = compare(&before, &after);
    let world: Vec<_> = changes
        .iter()
        .filter(|c| c.world)
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(world, ["world/region/r.0.0.mca", "world_nether/level.dat"]);
    assert!(
        changes
            .iter()
            .any(|c| c.path == "logs/latest.log" && !c.world)
    );
    assert!(compare(&after, &after).is_empty());
}

#[test]
fn removed_files_are_changes() {
    let before = scan_of(&[("world/level.dat", 1), ("world/data/x", 1)]);
    let after = scan_of(&[("world/level.dat", 1)]);
    let changes = compare(&before, &after);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].world);
}
