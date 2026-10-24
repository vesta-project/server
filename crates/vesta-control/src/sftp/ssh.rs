use super::*;
use russh::{
    Channel, ChannelId,
    server::{Auth, ChannelOpenHandle, Msg, Session},
};
use std::{net::IpAddr, time::Duration};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

struct ConnectionPermit {
    service: Arc<SftpService>,
    ip: IpAddr,
}
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        if let Ok(mut b) = self.service.budgets.lock()
            && let Some(n) = b.sessions.get_mut(&self.ip)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                b.sessions.remove(&self.ip);
            }
        }
    }
}
fn admit(service: &Arc<SftpService>, config: &Config, ip: IpAddr) -> Option<ConnectionPermit> {
    if !config.permits(ip) {
        return None;
    }
    let mut b = service.budgets.lock().ok()?;
    let now = Instant::now();
    b.attempts
        .retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
    if !b.attempts.contains_key(&ip) && b.attempts.len() >= 1024 {
        return None;
    }
    let (since, count) = b.attempts.entry(ip).or_insert((now, 0));
    if now.duration_since(*since) >= Duration::from_secs(60) {
        *since = now;
        *count = 0;
    }
    *count = count.saturating_add(1);
    if *count > 12 {
        return None;
    }
    let total: usize = b.sessions.values().sum();
    if total >= config.limits.sessions
        || b.sessions.get(&ip).copied().unwrap_or(0) >= config.limits.sessions_per_ip
    {
        return None;
    }
    *b.sessions.entry(ip).or_insert(0) += 1;
    Some(ConnectionPermit {
        service: service.clone(),
        ip,
    })
}

pub(super) async fn listen(
    service: std::sync::Weak<SftpService>,
    listener: TcpListener,
    config: Config,
    key: PrivateKey,
    mut stop: watch::Receiver<bool>,
) {
    let ssh_config = Arc::new(russh::server::Config {
        keys: vec![key],
        methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_millis(100)),
        max_auth_attempts: 4,
        inactivity_timeout: Some(Duration::from_secs(config.limits.idle_timeout_secs)),
        window_size: 128 * 1024,
        maximum_packet_size: 32 * 1024,
        channel_buffer_size: 4,
        event_buffer_size: 4,
        keepalive_interval: None,
        ..Default::default()
    });
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _=stop.changed()=>break,
            result=connections.join_next(),if !connections.is_empty()=>{let _=result;},
            result=listener.accept()=> {
                let Ok((socket,peer))=result else {break;};
                let Some(service)=service.upgrade() else {break;};
                if let Some(permit)=admit(&service,&config,peer.ip()) {
                    connections.spawn(connection(service.clone(),socket,config.clone(),ssh_config.clone(),stop.clone(),permit));
                }
            }
        }
    }
    // Each bridge observes stop and closes its TCP stream. Drain existing work
    // before the service reports shutdown complete; never detach a listener.
    drop(listener);
    while connections.join_next().await.is_some() {}
}

async fn connection(
    service: Arc<SftpService>,
    mut socket: TcpStream,
    config: Config,
    ssh_config: Arc<russh::server::Config>,
    mut stop: watch::Receiver<bool>,
    _permit: ConnectionPermit,
) {
    if *stop.borrow() {
        return;
    }
    let (transport, mut bridge) = tokio::io::duplex(128 * 1024);
    let mut bridge_stop = stop.clone();
    let bridge_task = tokio::spawn(async move {
        tokio::select! {biased; _=bridge_stop.changed()=>{},_ = copy_bidirectional(&mut socket,&mut bridge)=>{}}
    });
    let handler = Ssh {
        service,
        config,
        subject: None,
        channel: None,
        channel_used: false,
        protocol: None,
        stop: stop.clone(),
    };
    let started = tokio::select! {biased; _=stop.changed()=>None, result=tokio::time::timeout(Duration::from_secs(15),russh::server::run_stream(ssh_config,transport,handler))=>result.ok().and_then(std::result::Result::ok)};
    if let Some(mut session) = started {
        tokio::select! {
            biased;
            _=stop.changed()=> {let _=session.handle().disconnect(russh::Disconnect::ByApplication,"Service stopped".into(),"en".into()).await; let _=tokio::time::timeout(Duration::from_secs(2),&mut session).await;},
            _=&mut session=>{}
        }
    }
    bridge_task.abort();
    let _ = bridge_task.await;
}

struct Ssh {
    service: Arc<SftpService>,
    config: Config,
    subject: Option<Subject>,
    channel: Option<Channel<Msg>>,
    channel_used: bool,
    protocol: Option<JoinHandle<()>>,
    stop: watch::Receiver<bool>,
}
impl Drop for Ssh {
    fn drop(&mut self) {
        if let Some(task) = self.protocol.take() {
            task.abort();
        }
    }
}
impl russh::server::Handler for Ssh {
    type Error = russh::Error;
    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> std::result::Result<Auth, Self::Error> {
        let user = user.to_owned();
        let fp = key.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        let result = self
            .service
            .blocking(move |s| s.credential(&user, &fp))
            .await;
        Ok(if result.is_ok() {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> std::result::Result<Auth, Self::Error> {
        let user = user.to_owned();
        let fp = key.fingerprint(ssh_key::HashAlg::Sha256).to_string();
        let result = self
            .service
            .blocking(move |s| {
                let subject = s.credential(&user, &fp)?;
                s.db.write_with(|tx| {
                    s.record(tx.connection(), &subject, "sftp.authenticate", "success")
                })
                .map_err(Error::from)?;
                Ok(subject)
            })
            .await;
        match result {
            Ok(subject) => {
                self.subject = Some(subject);
                Ok(Auth::Accept)
            }
            Err(_) => Ok(Auth::reject()),
        }
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        if !self.channel_used
            && self
                .subject
                .as_ref()
                .is_some_and(|s| self.service.recheck(s).is_ok())
        {
            self.channel_used = true;
            self.channel = Some(channel);
            reply.accept().await;
        }
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        if name == "sftp"
            && self.protocol.is_none()
            && let (Some(channel), Some(subject)) = (self.channel.take(), self.subject.clone())
        {
            session.channel_success(id)?;
            let service = self.service.clone();
            let config = self.config.clone();
            let stop = self.stop.clone();
            self.protocol = Some(tokio::spawn(async move {
                protocol::run(service, subject, config, channel.into_stream(), stop).await;
            }));
            return Ok(());
        }
        session.channel_failure(id)?;
        Ok(())
    }
    async fn channel_eof(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.close(id)?;
        Ok(())
    }
    async fn channel_close(
        &mut self,
        _: ChannelId,
        _: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        if let Some(task) = self.protocol.take() {
            task.abort();
        }
        Ok(())
    }
    async fn shell_request(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_failure(id)?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: ChannelId,
        _: &[u8],
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_failure(id)?;
        Ok(())
    }
    async fn env_request(
        &mut self,
        id: ChannelId,
        _: &str,
        _: &str,
        session: &mut Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_failure(id)?;
        Ok(())
    }
    // The default SSH handlers reject exec, shell, PTY, forwarding, X11,
    // agent forwarding and every other channel type. None is overridden.
}
