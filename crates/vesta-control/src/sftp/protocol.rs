//! Owned bounded SFTP v3 framing using the maintained russh-sftp wire codec.
//! An invalid/oversized frame closes the channel. No detached retry/error loop.
use super::*;
use crate::files::{
    EntryKind, FILE_CHUNK_BYTES, FileEntry, FileError, FileStream, RelativePath, SftpWrite,
};
use bytes::Bytes;
use russh_sftp::protocol::*;
use std::time::{Duration, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_PACKET: u32 = 128 * 1024;
struct Transfer {
    service: Arc<SftpService>,
    principal: String,
}
impl Drop for Transfer {
    fn drop(&mut self) {
        if let Ok(mut b) = self.service.budgets.lock()
            && let Some(n) = b.transfers.get_mut(&self.principal)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                b.transfers.remove(&self.principal);
            }
        }
    }
}
fn transfer(
    service: &Arc<SftpService>,
    subject: &Subject,
    limit: usize,
) -> std::result::Result<Transfer, StatusCode> {
    let mut b = service.budgets.lock().map_err(|_| StatusCode::Failure)?;
    let n = b
        .transfers
        .entry(subject.principal.as_str().into())
        .or_insert(0);
    if *n >= limit {
        return Err(StatusCode::Failure);
    }
    *n += 1;
    Ok(Transfer {
        service: service.clone(),
        principal: subject.principal.as_str().into(),
    })
}
enum Opened {
    Read {
        path: RelativePath,
        stream: FileStream,
        _transfer: Transfer,
    },
    Write {
        handle: SftpWrite,
        append: bool,
        readable: bool,
        _transfer: Transfer,
    },
    Directory {
        path: Option<RelativePath>,
        offset: usize,
    },
}
struct State {
    handles: HashMap<String, Opened>,
}
fn status(id: u32) -> Packet {
    Packet::Status(Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: "en".into(),
    })
}
fn file_error(e: FileError) -> StatusCode {
    match e {
        FileError::NotFound => StatusCode::NoSuchFile,
        FileError::InvalidPath
        | FileError::ProtectedPath
        | FileError::LinkedPath
        | FileError::Control(_) => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}
fn path(raw: &str) -> std::result::Result<Option<RelativePath>, StatusCode> {
    if raw.is_empty() || raw == "." || raw == "/" || raw == "/." {
        return Ok(None);
    }
    if raw.starts_with("//") || raw.len() > crate::files::MAX_PATH_BYTES + 1 {
        return Err(StatusCode::PermissionDenied);
    }
    let text = raw.strip_prefix('/').unwrap_or(raw);
    RelativePath::parse(text).map(Some).map_err(file_error)
}
fn leaf(raw: &str) -> std::result::Result<RelativePath, StatusCode> {
    path(raw)?.ok_or(StatusCode::PermissionDenied)
}
fn attrs(entry: &FileEntry) -> FileAttributes {
    let modified = entry
        .modified_at
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| u32::try_from(d.as_secs()).ok());
    let mut a = FileAttributes {
        size: Some(entry.size),
        mtime: modified,
        atime: modified,
        ..Default::default()
    };
    // Only type bits are exposed. Unix permission ownership is not fabricated.
    if entry.kind == EntryKind::Directory {
        a.set_dir(true)
    } else {
        a.set_regular(true)
    }
    a
}

pub(super) async fn run<S: AsyncRead + AsyncWrite + Unpin>(
    service: Arc<SftpService>,
    subject: Subject,
    config: Config,
    mut stream: S,
    mut stop: watch::Receiver<bool>,
) {
    let state = Arc::new(Mutex::new(State {
        handles: HashMap::new(),
    }));
    let mut initialized = false;
    let mut total = 0u64;
    let mut requests = (Instant::now(), 0u32);
    loop {
        let incoming = async {
            let size = stream.read_u32().await?;
            if size == 0 || size > MAX_PACKET {
                return Err(std::io::Error::other("packet limit"));
            }
            let mut data = vec![0; size as usize];
            stream.read_exact(&mut data).await?;
            Packet::try_from(&mut Bytes::from(data))
                .map_err(|_| std::io::Error::other("invalid packet"))
        };
        let packet = tokio::select! {biased;_ = stop.changed()=>break,result=tokio::time::timeout(Duration::from_secs(config.limits.idle_timeout_secs),incoming)=>match result {Ok(Ok(p))=>p,_=>break}};
        let now = Instant::now();
        if now.duration_since(requests.0) >= Duration::from_secs(1) {
            requests = (now, 0);
        }
        requests.1 += 1;
        if requests.1 > 256 {
            break;
        }
        let id = packet.get_request_id();
        let charge = match &packet {
            Packet::Write(w) => w.data.len() as u64,
            Packet::Read(r) => u64::from(r.len.min(FILE_CHUNK_BYTES as u32)),
            _ => 0,
        };
        total = total.saturating_add(charge);
        if total > config.limits.bytes_per_session {
            break;
        }
        if let Ok(delay) = service.rate_delay(&subject, charge, config.limits.bytes_per_second) {
            tokio::select! {biased;_=stop.changed()=>break,_=tokio::time::sleep(delay)=>{}}
        } else {
            break;
        }
        let response = if let Packet::Init(_) = &packet {
            if initialized {
                break;
            }
            initialized = true;
            Packet::Version(Version::new())
        } else {
            if !initialized {
                break;
            }
            let state = state.clone();
            let subject = subject.clone();
            let limits = config.limits.clone();
            let result = service
                .blocking(move |s| {
                    s.recheck(&subject)?;
                    s.reserve_traffic(&subject, charge, limits.bytes_per_principal_day)?;
                    let mut state = state.lock().map_err(|_| Error("sftp_unavailable"))?;
                    let result = process(&s, &subject, &limits, &mut state, packet);
                    match &result {
                        Ok(_) => {}
                        Err(_) => {
                            s.db.write_with(|tx| {
                                s.record(tx.connection(), &subject, "sftp.file.denied", "denied")
                            })
                            .map_err(Error::from)?;
                        }
                    }
                    Ok(result.unwrap_or_else(|code| Packet::error(id, code)))
                })
                .await;
            match result {
                Ok(p) => p,
                Err(_) => Packet::error(id, StatusCode::PermissionDenied),
            }
        };
        let Ok(encoded) = Bytes::try_from(response) else {
            break;
        };
        let sent = tokio::select! {biased;_=stop.changed()=>break,result=tokio::time::timeout(Duration::from_secs(15),stream.write_all(&encoded))=>result};
        if !matches!(sent, Ok(Ok(()))) {
            break;
        }
    }
    // Dropping state releases canonical streams and transfer permits. No upload
    // staging exists to leak; the already visible partial target remains.
    let _ = stream.shutdown().await;
}

fn process(
    service: &Arc<SftpService>,
    subject: &Subject,
    limits: &Limits,
    state: &mut State,
    packet: Packet,
) -> std::result::Result<Packet, StatusCode> {
    let files = &service.files;
    let max = limits.max_file_bytes;
    let result = match packet {
        Packet::Open(p) => {
            if state.handles.len() >= limits.handles_per_session {
                return Err(StatusCode::Failure);
            }
            let path = leaf(&p.filename)?;
            if p.pflags.bits() & !0x3f != 0
                || !p.pflags.intersects(OpenFlags::READ | OpenFlags::WRITE)
                || (p.pflags.intersects(
                    OpenFlags::CREATE
                        | OpenFlags::TRUNCATE
                        | OpenFlags::APPEND
                        | OpenFlags::EXCLUDE,
                ) && !p.pflags.contains(OpenFlags::WRITE))
            {
                return Err(StatusCode::BadMessage);
            }
            if p.attrs.uid.is_some() || p.attrs.gid.is_some() {
                return Err(StatusCode::PermissionDenied);
            }
            let permit = transfer(service, subject, limits.transfers_per_principal)?;
            let opened = if p.pflags.contains(OpenFlags::WRITE) {
                if p.pflags.contains(OpenFlags::READ) {
                    let _ = files.open_stream(subject, &path).map_err(file_error)?;
                }
                let handle = files
                    .sftp_open_write(
                        subject,
                        path,
                        p.pflags.contains(OpenFlags::CREATE),
                        p.pflags.contains(OpenFlags::TRUNCATE),
                        p.pflags.contains(OpenFlags::EXCLUDE),
                        max,
                    )
                    .map_err(file_error)?;
                Opened::Write {
                    handle,
                    append: p.pflags.contains(OpenFlags::APPEND),
                    readable: p.pflags.contains(OpenFlags::READ),
                    _transfer: permit,
                }
            } else {
                Opened::Read {
                    stream: files.open_stream(subject, &path).map_err(file_error)?,
                    path,
                    _transfer: permit,
                }
            };
            let handle = uuid::Uuid::new_v4().to_string();
            state.handles.insert(handle.clone(), opened);
            Packet::Handle(Handle { id: p.id, handle })
        }
        Packet::Close(p) => {
            state.handles.remove(&p.handle).ok_or(StatusCode::Failure)?;
            status(p.id)
        }
        Packet::Read(p) => {
            if p.len == 0 || p.len as usize > FILE_CHUNK_BYTES {
                return Err(StatusCode::Failure);
            }
            let data = match state
                .handles
                .get_mut(&p.handle)
                .ok_or(StatusCode::Failure)?
            {
                Opened::Read { stream, .. } => {
                    files
                        .reauthorize_stream(subject, stream)
                        .map_err(file_error)?;
                    stream
                        .read_chunk(p.offset, p.len as usize)
                        .map_err(file_error)?
                }
                Opened::Write {
                    handle,
                    readable: true,
                    ..
                } => {
                    let mut stream = files
                        .open_stream(subject, &handle.path)
                        .map_err(file_error)?;
                    stream
                        .read_chunk(p.offset, p.len as usize)
                        .map_err(file_error)?
                }
                _ => return Err(StatusCode::PermissionDenied),
            };
            if data.is_empty() {
                return Err(StatusCode::Eof);
            }
            Packet::Data(Data { id: p.id, data })
        }
        Packet::Write(p) => {
            match state
                .handles
                .get_mut(&p.handle)
                .ok_or(StatusCode::Failure)?
            {
                Opened::Write { handle, append, .. } => files
                    .sftp_write_chunk(subject, handle, p.offset, &p.data, *append, max)
                    .map_err(file_error)?,
                _ => return Err(StatusCode::PermissionDenied),
            }
            status(p.id)
        }
        Packet::Stat(p) => Packet::Attrs(Attrs {
            id: p.id,
            attrs: attrs(
                &files
                    .sftp_stat(subject, path(&p.path)?.as_ref())
                    .map_err(file_error)?,
            ),
        }),
        Packet::Lstat(p) => Packet::Attrs(Attrs {
            id: p.id,
            attrs: attrs(
                &files
                    .sftp_stat(subject, path(&p.path)?.as_ref())
                    .map_err(file_error)?,
            ),
        }),
        Packet::Fstat(p) => {
            let path = match state.handles.get(&p.handle).ok_or(StatusCode::Failure)? {
                Opened::Read { path, .. } => Some(path),
                Opened::Write { handle, .. } => Some(&handle.path),
                Opened::Directory { path, .. } => path.as_ref(),
            };
            Packet::Attrs(Attrs {
                id: p.id,
                attrs: attrs(&files.sftp_stat(subject, path).map_err(file_error)?),
            })
        }
        Packet::OpenDir(p) => {
            if state.handles.len() >= limits.handles_per_session {
                return Err(StatusCode::Failure);
            }
            let path = path(&p.path)?;
            let _ = files.list(subject, path.as_ref()).map_err(file_error)?;
            let handle = uuid::Uuid::new_v4().to_string();
            state
                .handles
                .insert(handle.clone(), Opened::Directory { path, offset: 0 });
            Packet::Handle(Handle { id: p.id, handle })
        }
        Packet::ReadDir(p) => {
            let Opened::Directory { path, offset } = state
                .handles
                .get_mut(&p.handle)
                .ok_or(StatusCode::Failure)?
            else {
                return Err(StatusCode::Failure);
            };
            // Reload every page so revocation/role/scope changes never expose a
            // cached listing. Directory mutation may alter pagination order.
            let listing = files.list(subject, path.as_ref()).map_err(file_error)?;
            let page = listing
                .entries
                .iter()
                .skip(*offset)
                .take(100)
                .map(|e| File::new(e.name.clone(), attrs(e)))
                .collect::<Vec<_>>();
            if page.is_empty() {
                return Err(StatusCode::Eof);
            }
            *offset += page.len();
            Packet::Name(Name {
                id: p.id,
                files: page,
            })
        }
        Packet::RealPath(p) => {
            let path = path(&p.path)?;
            let e = files
                .sftp_stat(subject, path.as_ref())
                .map_err(file_error)?;
            Packet::Name(Name {
                id: p.id,
                files: vec![File::new(
                    path.map(|p| format!("/{}", p.as_str()))
                        .unwrap_or("/".into()),
                    attrs(&e),
                )],
            })
        }
        Packet::MkDir(p) => {
            if p.attrs.uid.is_some() || p.attrs.gid.is_some() {
                return Err(StatusCode::PermissionDenied);
            }
            files.mkdir(subject, &leaf(&p.path)?).map_err(file_error)?;
            status(p.id)
        }
        Packet::Remove(p) => {
            files
                .sftp_remove(subject, &leaf(&p.filename)?, max)
                .map_err(file_error)?;
            status(p.id)
        }
        Packet::RmDir(p) => {
            files
                .remove_empty_directory(subject, &leaf(&p.path)?)
                .map_err(file_error)?;
            status(p.id)
        }
        Packet::Rename(p) => {
            files
                .sftp_rename(subject, &leaf(&p.oldpath)?, &leaf(&p.newpath)?, max)
                .map_err(file_error)?;
            status(p.id)
        }
        _ => return Err(StatusCode::OpUnsupported),
    };
    Ok(result)
}
