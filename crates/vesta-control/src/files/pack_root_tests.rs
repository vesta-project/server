use super::*;
use std::io::{self, Cursor};

#[test]
fn exact_and_empty_streams_keep_independent_digest_goldens() {
    assert_eq!(
        pin_bytes(&mut Cursor::new(b"abc"), 3, 3).unwrap(),
        (
            b"abc".to_vec(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()
        )
    );
    assert_eq!(
        pin_bytes(&mut Cursor::new(b""), 0, 0).unwrap(),
        (
            vec![],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()
        )
    );
}

#[test]
fn short_or_growing_streams_never_return_a_partial_or_new_artifact() {
    assert_eq!(
        pin_bytes(&mut Cursor::new(b"ab"), 3, 3).unwrap_err(),
        FileError::Unavailable
    );
    let mut growing = Cursor::new(b"abcdef");
    assert_eq!(
        pin_bytes(&mut growing, 3, 8).unwrap_err(),
        FileError::Unavailable
    );
    assert_eq!(growing.position(), 4);
    let mut over = Cursor::new(b"abcdef");
    assert_eq!(pin_bytes(&mut over, 3, 3).unwrap_err(), FileError::TooLarge);
    assert_eq!(over.position(), 4);
    let mut poison = Cursor::new(b"never touched");
    assert_eq!(
        pin_bytes(&mut poison, 4, 3).unwrap_err(),
        FileError::TooLarge
    );
    assert_eq!(poison.position(), 0);
}

#[test]
fn one_byte_reads_and_interrupts_hash_the_exact_stream_without_recursion() {
    struct Chunks {
        position: usize,
        interrupted: bool,
    }
    impl Read for Chunks {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.interrupted = false;
            if self.position == 3 {
                return Ok(0);
            }
            into[0] = b"abc"[self.position];
            self.position += 1;
            Ok(1)
        }
    }
    let (_, hash) = pin_bytes(
        &mut Chunks {
            position: 0,
            interrupted: false,
        },
        3,
        3,
    )
    .unwrap();
    assert_eq!(
        hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn native_io_failures_discard_partial_bytes_and_diagnostics() {
    struct Failure {
        cursor: Cursor<&'static [u8]>,
        at: u64,
    }
    impl Read for Failure {
        fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
            if self.cursor.position() == self.at {
                return Err(io::Error::other("private-path-secret"));
            }
            let allowed = (self.at - self.cursor.position()) as usize;
            let size = into.len().min(allowed);
            self.cursor.read(&mut into[..size])
        }
    }
    for at in 0..=3 {
        let error = pin_bytes(
            &mut Failure {
                cursor: Cursor::new(b"abc"),
                at,
            },
            3,
            3,
        )
        .unwrap_err();
        assert_eq!(error, FileError::Unavailable);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
}
