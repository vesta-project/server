//! Disk-backed range bodies use the artifact throughput floor and fixed chunks.
use crate::{BODY_CHUNK, ConnectionDeadline};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use std::{
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use vesta_control::files::FileStream;

struct Stream {
    file: FileStream,
    offset: u64,
    remaining: u64,
    permit: Arc<OwnedSemaphorePermit>,
    /// Downloads tolerate appends after they start; ranges require an unchanged file.
    growing: bool,
}
pub(super) struct FileBody {
    stream: Option<Stream>,
    pending: Option<JoinHandle<(Stream, io::Result<Bytes>)>>,
    deadline: Option<Arc<ConnectionDeadline>>,
}
struct Chunk {
    bytes: Vec<u8>,
    _permit: Arc<OwnedSemaphorePermit>,
}
impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl FileBody {
    pub(super) fn new(file: FileStream, offset: u64, count: u64) -> Option<Self> {
        Self::build(file, offset, count, false)
    }
    /// A whole-file download of the bytes present when it started, of any size.
    pub(super) fn download(file: FileStream, offset: u64, count: u64) -> Option<Self> {
        Self::build(file, offset, count, true)
    }
    fn build(file: FileStream, offset: u64, count: u64, growing: bool) -> Option<Self> {
        static BUDGET: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = BUDGET
            .get_or_init(|| Arc::new(Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
            .ok()?;
        Some(Self {
            stream: Some(Stream {
                file,
                offset,
                remaining: count,
                permit: Arc::new(permit),
                growing,
            }),
            pending: None,
            deadline: None,
        })
    }
    pub(super) fn watch(&mut self, deadline: Arc<ConnectionDeadline>) {
        deadline.stream();
        self.deadline = Some(deadline);
    }
}
impl Drop for FileBody {
    fn drop(&mut self) {
        if let Some(deadline) = &self.deadline {
            deadline.finish();
        }
    }
}
impl Body for FileBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<io::Result<Frame<Bytes>>>> {
        if self.pending.is_none() {
            let Some(mut stream) = self.stream.take() else {
                return Poll::Ready(None);
            };
            if stream.remaining == 0 {
                return Poll::Ready(None);
            }
            self.pending = Some(tokio::task::spawn_blocking(move || {
                let count = stream.remaining.min(BODY_CHUNK as u64) as usize;
                let result = (|| {
                    let intact = if stream.growing {
                        stream.file.still_covers()
                    } else {
                        stream.file.unchanged()
                    };
                    if !intact.map_err(|_| io::Error::other("file changed"))? {
                        return Err(io::Error::other("file changed"));
                    }
                    let bytes = stream
                        .file
                        .read_chunk(stream.offset, count)
                        .map_err(|_| io::Error::other("file read failed"))?;
                    let intact = if stream.growing {
                        stream.file.still_covers()
                    } else {
                        stream.file.unchanged()
                    };
                    if !intact.map_err(|_| io::Error::other("file changed"))? {
                        return Err(io::Error::other("file changed"));
                    }
                    Ok(bytes)
                })()
                .map(|bytes| {
                    stream.offset += bytes.len() as u64;
                    stream.remaining -= bytes.len() as u64;
                    Bytes::from_owner(Chunk {
                        bytes,
                        _permit: stream.permit.clone(),
                    })
                });
                (stream, result)
            }));
        }
        let result = match Pin::new(self.pending.as_mut().expect("read task")).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        self.pending = None;
        match result {
            Ok((stream, Ok(bytes))) => {
                if stream.remaining > 0 {
                    self.stream = Some(stream);
                }
                if let Some(deadline) = &self.deadline {
                    deadline.progress(bytes.len());
                }
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            _ => Poll::Ready(Some(Err(io::Error::other("file read failed")))),
        }
    }
    fn is_end_stream(&self) -> bool {
        self.pending.is_none() && self.stream.as_ref().is_none_or(|s| s.remaining == 0)
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use vesta_control::{
        files::{FileBrowser, RelativePath},
        identity::{DeviceId, PrincipalId, Subject},
    };
    fn file_fixture(chunks: usize) -> (tempfile::TempDir, FileBrowser, Subject) {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        std::fs::write(game.join("latest.log"), vec![b'x'; chunks * BODY_CHUNK]).unwrap();
        let db = vesta_state::StateDatabase::open(
            vesta_state::StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            Arc::new(vesta_state::SystemClock),
        )
        .unwrap();
        let subject = Subject {
            principal: PrincipalId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
            device: DeviceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap(),
        };
        db.write(|c| {
            let now = db.clock().millis();
            c.execute("INSERT INTO principals (id,server_id,display_name,status,created_at,updated_at) VALUES (?,?,'Fixture','active',?,?)", rusqlite::params![subject.principal.as_str(),db.server_id(),now,now])?;
            c.execute("INSERT INTO devices (id,server_id,principal_id,label,public_key_sha256,created_at) VALUES (?,?,?,'Fixture',?,?)", rusqlite::params![subject.device.as_str(),db.server_id(),subject.principal.as_str(),"0".repeat(64),now])?;
            c.execute("INSERT INTO role_assignments (id,principal_id,role_id,granted_by,created_at) VALUES ('cccccccc-cccc-4ccc-8ccc-cccccccccccc',?,'owner','fixture',?)", rusqlite::params![subject.principal.as_str(),now])?;
            Ok(())
        }).unwrap();
        let browser = FileBrowser::open(db, &game, &[]).unwrap();
        (temp, browser, subject)
    }
    #[test]
    fn several_mib_range_body_has_no_read_ahead_or_file_sized_allocation() {
        let (_temp, browser, subject) = file_fixture(81);
        let file = browser
            .open_stream(&subject, &RelativePath::parse("latest.log").unwrap())
            .unwrap();
        let mut body = FileBody::new(file, 0, 4 * 1024 * 1024).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut frames = 0;
            let mut total = 0;
            while let Some(frame) = body.frame().await {
                let bytes = frame.unwrap().into_data().unwrap();
                assert_eq!(bytes.len(), BODY_CHUNK);
                assert!(body.pending.is_none());
                assert!(
                    body.stream
                        .as_ref()
                        .is_none_or(|stream| stream.remaining <= 4 * 1024 * 1024)
                );
                total += bytes.len();
                frames += 1;
            }
            assert_eq!(frames, 64);
            assert_eq!(total, 4 * 1024 * 1024);
        });
    }
    #[test]
    fn file_range_detects_changed_resource_and_aborts_before_completion() {
        let (temp, browser, subject) = file_fixture(5);
        let file = browser
            .open_stream(&subject, &RelativePath::parse("latest.log").unwrap())
            .unwrap();
        let mut body = FileBody::new(file, 0, (5 * BODY_CHUNK) as u64).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert_eq!(
                body.frame()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_data()
                    .unwrap()
                    .len(),
                BODY_CHUNK
            );
            std::fs::write(temp.path().join("game/latest.log"), b"truncated").unwrap();
            assert!(body.frame().await.unwrap().is_err());
        });
    }
}
