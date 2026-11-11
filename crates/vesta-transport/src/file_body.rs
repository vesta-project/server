//! Disk-backed range bodies use the artifact throughput floor and fixed chunks.
use crate::{BODY_CHUNK, ConnectionDeadline};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use sha2::{Digest, Sha256};
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
use vesta_control::{
    files::{FileBrowser, FileStream},
    identity::management::ManagementAccess,
};

struct Authorization {
    management: ManagementAccess,
    browser: FileBrowser,
    token: zeroize::Zeroizing<String>,
}
impl Authorization {
    fn check(&self, file: &FileStream) -> io::Result<()> {
        let subject = self
            .management
            .authenticate(&self.token)
            .map_err(|_| io::Error::other("download authorization expired"))?;
        self.browser
            .reauthorize_stream(&subject, file)
            .map_err(|_| io::Error::other("download authorization revoked"))
    }
}
struct Integrity {
    expected: [u8; 32],
    observed: Sha256,
}

struct Stream {
    file: FileStream,
    offset: u64,
    remaining: u64,
    permit: Arc<OwnedSemaphorePermit>,
    /// Downloads tolerate appends after they start; ranges require an unchanged file.
    growing: bool,
    authorization: Option<Authorization>,
    integrity: Option<Integrity>,
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
    pub(super) fn download(
        file: FileStream,
        offset: u64,
        count: u64,
    ) -> vesta_control::files::Result<Option<Self>> {
        let Some(mut body) = Self::build(file, offset, count, true) else {
            return Ok(None);
        };
        let stream = body.stream.as_mut().expect("new stream");
        let expected = stream.file.snapshot_digest(offset, count)?;
        stream.integrity = Some(Integrity {
            expected,
            observed: Sha256::new(),
        });
        Ok(Some(body))
    }
    pub(super) fn authorize(
        &mut self,
        management: ManagementAccess,
        browser: FileBrowser,
        token: &str,
    ) {
        self.stream.as_mut().expect("new stream").authorization = Some(Authorization {
            management,
            browser,
            token: zeroize::Zeroizing::new(token.to_owned()),
        });
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
                authorization: None,
                integrity: None,
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
                    if let Some(authorization) = &stream.authorization {
                        authorization.check(&stream.file)?;
                    }
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
                    if let Some(integrity) = &mut stream.integrity {
                        integrity.observed.update(&bytes);
                        if bytes.len() as u64 == stream.remaining
                            && <[u8; 32]>::from(integrity.observed.clone().finalize())
                                != integrity.expected
                        {
                            return Err(io::Error::other("file changed"));
                        }
                    }
                    if let Some(authorization) = &stream.authorization {
                        authorization.check(&stream.file)?;
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
    struct TestClock(std::sync::atomic::AtomicI64);
    impl vesta_state::Clock for TestClock {
        fn millis(&self) -> i64 {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    fn file_fixture(
        chunks: usize,
    ) -> (
        tempfile::TempDir,
        FileBrowser,
        Subject,
        vesta_state::StateDatabase,
        Arc<TestClock>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        std::fs::write(game.join("latest.log"), vec![b'x'; chunks * BODY_CHUNK]).unwrap();
        let clock = Arc::new(TestClock(std::sync::atomic::AtomicI64::new(
            1_791_115_200_000,
        )));
        let db = vesta_state::StateDatabase::open(
            vesta_state::StateDirectory::new(temp.path().join("state"), Some(&game)).unwrap(),
            clock.clone(),
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
        let browser = FileBrowser::open(db.clone(), &game, &[]).unwrap();
        (temp, browser, subject, db, clock)
    }
    #[test]
    fn several_mib_range_body_has_no_read_ahead_or_file_sized_allocation() {
        let (_temp, browser, subject, _db, _clock) = file_fixture(81);
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
        let (temp, browser, subject, _db, _clock) = file_fixture(5);
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
    #[test]
    fn downloads_reauthorize_device_capability_exact_resource_and_session_between_chunks() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ring::signature::KeyPair;
        use vesta_control::identity::{BuiltinRole, management::ManagementError};
        for refusal in ["device", "capability", "scope", "session", "account"] {
            let (temp, browser, _subject, db, clock) = file_fixture(3);
            let management = ManagementAccess::new(db.clone()).unwrap();
            let key = ring::signature::Ed25519KeyPair::from_pkcs8(
                ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                    .unwrap()
                    .as_ref(),
            )
            .unwrap();
            let code = management.issue_local(BuiltinRole::Owner, 10).unwrap();
            let enrolled = management
                .enroll(
                    code.secret.expose(),
                    "Download fixture",
                    &URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                )
                .unwrap();
            assert!(matches!(
                management.challenge(&enrolled.device_id),
                Err(ManagementError::SessionInvalid)
            ));
            // Like the HTTPS fixtures, seed an account/device binding locally.
            // Verified Mojang enrollment is covered by the control identity tests;
            // an unbound legacy device must never enter this download session.
            let account = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
            db.write(|c| {
                c.execute(
                    "INSERT INTO user_minecraft_accounts(server_id,minecraft_uuid,principal_id) VALUES(?,?,?)",
                    rusqlite::params![enrolled.server_id, account, enrolled.principal_id],
                )?;
                c.execute(
                    "UPDATE management_device_keys SET linked_minecraft_uuid=? WHERE device_id=?",
                    rusqlite::params![account, enrolled.device_id],
                )?;
                Ok(())
            }).unwrap();
            let nonce = management.challenge(&enrolled.device_id).unwrap();
            let signature = key.sign(&vesta_control::identity::management::session_message(
                management.server_id(),
                nonce.expose(),
            ));
            let session = management
                .session(
                    &enrolled.device_id,
                    nonce.expose(),
                    &URL_SAFE_NO_PAD.encode(signature.as_ref()),
                )
                .unwrap();
            let subject = management.authenticate(session.secret.expose()).unwrap();
            let file = browser
                .open_stream(&subject, &RelativePath::parse("latest.log").unwrap())
                .unwrap();
            let mut body = FileBody::download(file, 0, (3 * BODY_CHUNK) as u64)
                .unwrap()
                .unwrap();
            body.authorize(management.clone(), browser.clone(), session.secret.expose());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                assert!(body.frame().await.unwrap().is_ok());
                std::fs::rename(temp.path().join("game/latest.log"), temp.path().join("game/rotated.log")).unwrap();
                std::fs::write(temp.path().join("game/latest.log"), vec![b'y'; 3 * BODY_CHUNK]).unwrap();
                match refusal {
                    "device" => management.revoke_local(&enrolled.device_id).unwrap(),
                    "capability" => db.write(|c| {
                        c.execute("UPDATE role_assignments SET revoked_at=1 WHERE principal_id=?", [subject.principal.as_str()])?;
                        Ok(())
                    }).unwrap(),
                    "scope" => db.write(|c| {
                        c.execute("INSERT INTO principal_overrides(id,principal_id,capability,scope,effect,granted_by,created_at) VALUES('download-deny',?,'files.read','latest.log','deny','test',0)", [subject.principal.as_str()])?;
                        Ok(())
                    }).unwrap(),
                    "session" => { clock.0.fetch_add(24 * 60 * 60 * 1000, std::sync::atomic::Ordering::SeqCst); },
                    "account" => db.write(|c| {
                        c.execute("DELETE FROM user_minecraft_accounts WHERE principal_id=?", [&enrolled.principal_id])?;
                        Ok(())
                    }).unwrap(),
                    _ => unreachable!(),
                }
                assert!(body.frame().await.unwrap().is_err(), "{refusal}");
                assert!(body.frame().await.is_none());
            });
        }
    }

    #[test]
    fn whole_downloads_tolerate_only_initial_bytes_and_abort_mixed_versions() {
        use std::io::{Seek, Write};
        for change in [
            "append",
            "overwrite",
            "truncate-regrow",
            "overwrite-append",
            "rotate",
        ] {
            let (temp, browser, subject, _db, _clock) = file_fixture(3);
            let path = temp.path().join("game/latest.log");
            let file = browser
                .open_stream(&subject, &RelativePath::parse("latest.log").unwrap())
                .unwrap();
            let mut body = FileBody::download(file, 0, (3 * BODY_CHUNK) as u64)
                .unwrap()
                .unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut bytes = body
                    .frame()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_data()
                    .unwrap()
                    .to_vec();
                match change {
                    "append" => std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap()
                        .write_all(b"new bytes ignored")
                        .unwrap(),
                    "overwrite" | "overwrite-append" => {
                        let mut writer =
                            std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                        writer
                            .seek(std::io::SeekFrom::Start((BODY_CHUNK + 7) as u64))
                            .unwrap();
                        writer.write_all(b"rewritten").unwrap();
                        if change == "overwrite-append" {
                            writer.seek(std::io::SeekFrom::End(0)).unwrap();
                            writer.write_all(b"appended").unwrap();
                        }
                    }
                    "truncate-regrow" => std::fs::write(&path, vec![b'z'; 4 * BODY_CHUNK]).unwrap(),
                    "rotate" => {
                        std::fs::rename(&path, temp.path().join("game/rotated.log")).unwrap();
                        std::fs::write(&path, vec![b'y'; 3 * BODY_CHUNK]).unwrap();
                    }
                    _ => unreachable!(),
                }
                let mut failed = false;
                while let Some(frame) = body.frame().await {
                    match frame {
                        Ok(frame) => bytes.extend_from_slice(&frame.into_data().unwrap()),
                        Err(_) => {
                            failed = true;
                            break;
                        }
                    }
                }
                if matches!(change, "append" | "rotate") {
                    assert!(!failed, "{change}");
                    assert_eq!(bytes, vec![b'x'; 3 * BODY_CHUNK]);
                } else {
                    assert!(failed, "{change}");
                    assert!(bytes.len() < 3 * BODY_CHUNK);
                }
            });
        }
    }

    #[test]
    fn text_page_refuses_a_file_changed_after_scanning_instead_of_stale_etag() {
        let (temp, browser, subject, _db, _clock) = file_fixture(1);
        let path = temp.path().join("game/latest.log");
        for backwards in [false, true] {
            std::fs::write(&path, b"first\nsecond\n").unwrap();
            let mut file = browser
                .open_stream(&subject, &RelativePath::parse("latest.log").unwrap())
                .unwrap();
            let page = file
                .lines(if backwards { file.size() } else { 0 }, 2, backwards)
                .unwrap();
            std::fs::write(&path, b"different content\n").unwrap();
            let reply = crate::manage::files::text_page_reply(&file, page);
            assert_eq!(reply.status(), http::StatusCode::PRECONDITION_FAILED);
        }
    }
}
