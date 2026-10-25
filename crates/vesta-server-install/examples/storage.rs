//! Read-only disk enumeration for diagnosing installer storage detection.
fn main() {
    for disk in sysinfo::Disks::new_with_refreshed_list().list() {
        println!(
            "mount={:?} filesystem={:?} free={} kind={:?}",
            disk.mount_point(),
            disk.file_system(),
            disk.available_space(),
            disk.kind()
        );
    }
}
