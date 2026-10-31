//! Two files at a time: one scoped helper plus the already-owned install worker.
//! Never detach a transfer or retain a whole pack's downloaded result queue.
use super::*;

pub(super) fn pair_map<T: Sync, R: Send>(
    items: &[T],
    fetch: &(impl Fn(&T) -> Result<R> + Sync),
) -> Result<Vec<R>> {
    match items {
        [] => Ok(Vec::new()),
        [one] => Ok(vec![fetch(one)?]),
        [first, second] => std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("vesta-pack-download".into())
                .spawn_scoped(scope, || fetch(first));
            match worker {
                Ok(worker) => {
                    let second = fetch(second);
                    let first = worker
                        .join()
                        .map_err(|_| super::super::super::PackError::Unavailable)?;
                    Ok(vec![first?, second?])
                }
                // Resource-constrained hosts can always continue sequentially.
                Err(_) => Ok(vec![fetch(first)?, fetch(second)?]),
            }
        }),
        _ => Err(super::super::super::PackError::TooLarge.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    #[test]
    fn two_downloads_overlap_and_keep_manifest_order() {
        let entered = Mutex::new(0);
        let both = Condvar::new();
        let result = pair_map(&[1, 2], &|value| {
            let mut count = entered.lock().unwrap();
            *count += 1;
            both.notify_all();
            let (count, _) = both
                .wait_timeout_while(count, Duration::from_secs(3), |count| *count < 2)
                .unwrap();
            assert_eq!(*count, 2, "downloads were serialized");
            Ok(value * 10)
        })
        .unwrap();
        assert_eq!(result, [10, 20]);
    }
    #[test]
    fn failures_join_the_other_transfer_and_never_return_partial_results() {
        let finished = AtomicUsize::new(0);
        let result = pair_map(&[1, 2], &|value| {
            finished.fetch_add(1, Ordering::SeqCst);
            if *value == 1 {
                Err(ReleaseError::Conflict)
            } else {
                Ok(20)
            }
        });
        assert!(matches!(result, Err(ReleaseError::Conflict)));
        assert_eq!(finished.load(Ordering::SeqCst), 2);
        assert!(pair_map(&[1, 2, 3], &|_| Ok(())).is_err());
    }
    #[test]
    fn collection_accepts_large_pack_without_retaining_payload_bytes() {
        let mut files = Selected::new();
        let mut used = 0;
        for i in 0..=vesta_protocol::pack::catalogue::MAX_FILES {
            insert_selected(
                &mut files,
                &mut used,
                format!("{i}"),
                Payload {
                    sha256: "a".repeat(64),
                    size: 1024 * 1024,
                },
                None,
            )
            .unwrap();
        }
        assert_eq!(files.len(), vesta_protocol::pack::catalogue::MAX_FILES + 1);
        assert_eq!(used, (vesta_protocol::pack::catalogue::MAX_FILES as u64 + 1) * 1024 * 1024);
        insert_selected(
            &mut files,
            &mut used,
            "0".into(),
            Payload {
                sha256: "b".repeat(64),
                size: 0,
            },
            None,
        )
        .unwrap();
        assert_eq!(used, vesta_protocol::pack::catalogue::MAX_FILES as u64 * 1024 * 1024);
    }
}
